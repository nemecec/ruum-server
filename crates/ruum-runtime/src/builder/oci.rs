//! Daemonless image assembly (card R-2b, "Daemonless image assembly"): the base image's
//! manifest and config, one new layer with the build's output under `/app`, and a new config
//! and manifest. No Docker daemon and no BuildKit: the layer is a tar written here
//! (deterministic headers: mtime 0, owner 10001, sorted entries), compressed with `flate2`, and
//! its digests are SHA-256 (`sha2`).
//!
//! The new config keeps the base image's `Env`, `architecture`, `os` and layers, and sets
//! `User: 10001`, `WorkingDir: /app`, no `Entrypoint`, `Cmd: ["/bin/sh", "-c", <start>]`; it
//! removes `Volumes`, `Healthcheck`, `OnBuild`, `ExposedPorts` and `StopSignal`, and its labels
//! are the builder's own (`ruum.app`, `ruum.build`, ...), never the app's.

use super::output::{Kind, OutEntry};
use bytes::Bytes;
use ruum_core::runtime::{OciDigest, RuntimeError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;

/// The owner of every file in the layer (the sandbox user).
pub const LAYER_UID: u64 = 10001;

/// The media type of the new layer.
pub const LAYER_MEDIA: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
/// The media type of the new config.
pub const CONFIG_MEDIA: &str = "application/vnd.oci.image.config.v1+json";

/// A content descriptor of a manifest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub digest: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
}

/// An image platform (OCI image index).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Platform {
    pub architecture: String,
    pub os: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// The OCI architecture of an engine (`GET /info` gives `x86_64` or `aarch64`).
pub fn engine_architecture(arch: &str) -> Result<&'static str, RuntimeError> {
    match arch {
        "x86_64" | "amd64" => Ok("amd64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => Err(RuntimeError::Internal {
            reason: format!("the engine's architecture {other:?} has no node22 base image"),
        }),
    }
}

#[derive(Deserialize)]
struct Index {
    manifests: Vec<Descriptor>,
}

/// The image manifest of `architecture` (linux) in an image index.
pub fn select_manifest(index: &[u8], architecture: &str) -> Result<Descriptor, RuntimeError> {
    let idx: Index = serde_json::from_slice(index).map_err(|e| RuntimeError::Internal {
        reason: format!("the base image index: {e}"),
    })?;
    idx.manifests
        .into_iter()
        .find(|d| {
            d.platform
                .as_ref()
                .is_some_and(|p| p.os == "linux" && p.architecture == architecture)
                && (d.media_type == super::registry::OCI_MANIFEST
                    || d.media_type == super::registry::DOCKER_MANIFEST)
        })
        .ok_or_else(|| RuntimeError::Internal {
            reason: format!("the base image index has no linux/{architecture} image"),
        })
}

/// An image manifest (the fields the builder reads).
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, RuntimeError> {
    serde_json::from_slice(bytes).map_err(|e| RuntimeError::Internal {
        reason: format!("the base image manifest: {e}"),
    })
}

/// The new layer: gzip bytes, their digest and the digest of the uncompressed tar (the tar
/// itself is not kept: the tests read the pushed layer back).
pub struct Layer {
    pub gz: Bytes,
    pub digest: OciDigest,
    pub diff_id: OciDigest,
}

/// A tar writer with deterministic headers: mtime 0, owner and group [`LAYER_UID`], no user or
/// group name. A path over the ustar limits gets a PAX `path` record, a link target over 100
/// bytes a PAX `linkpath` record.
pub struct TarWriter {
    out: Vec<u8>,
}

impl Default for TarWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl TarWriter {
    pub fn new() -> Self {
        Self { out: Vec::new() }
    }

    fn octal(field: &mut [u8], v: u64) {
        let digits = field.len() - 1;
        // The output cap keeps every size far below 8 GiB (11 octal digits).
        debug_assert!(
            u32::try_from(digits * 3).is_ok_and(|bits| bits >= 64 || v < 1u64 << bits),
            "a tar header field cannot hold {v}"
        );
        let s = format!("{v:0digits$o}");
        let s = &s.as_bytes()[s.len().saturating_sub(digits)..];
        field[..s.len()].copy_from_slice(s);
        field[digits] = 0;
    }

    fn header(
        name: &[u8],
        prefix: &[u8],
        mode: u32,
        size: u64,
        kind: u8,
        link: &[u8],
    ) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name);
        Self::octal(&mut h[100..108], u64::from(mode));
        Self::octal(&mut h[108..116], LAYER_UID);
        Self::octal(&mut h[116..124], LAYER_UID);
        Self::octal(&mut h[124..136], size);
        Self::octal(&mut h[136..148], 0);
        h[156] = kind;
        h[157..157 + link.len()].copy_from_slice(link);
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[345..345 + prefix.len()].copy_from_slice(prefix);
        h[148..156].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        h
    }

    fn pad(&mut self, len: u64) {
        let rem = (len % 512) as usize;
        if rem != 0 {
            self.out.extend(std::iter::repeat_n(0u8, 512 - rem));
        }
    }

    /// The ustar split of a path, when it fits.
    fn split(path: &[u8]) -> Option<(&[u8], &[u8])> {
        if path.len() <= 100 {
            return Some((&[][..], path));
        }
        for i in (1..path.len()).rev() {
            if path[i] == b'/' && i <= 155 && path.len() - i - 1 <= 100 && path.len() - i > 1 {
                return Some((&path[..i], &path[i + 1..]));
            }
        }
        None
    }

    fn pax_record(key: &str, value: &[u8]) -> Vec<u8> {
        // "<len> <key>=<value>\n", where <len> counts itself.
        let body = key.len() + value.len() + 3;
        let mut len = body + 1;
        loop {
            let n = body + len.to_string().len();
            if n == len {
                break;
            }
            len = n;
        }
        let mut r = format!("{len} {key}=").into_bytes();
        r.extend_from_slice(value);
        r.push(b'\n');
        r
    }

    fn entry(&mut self, path: &[u8], mode: u32, kind: u8, link: &[u8], data: &[u8]) {
        let split = Self::split(path);
        let long_link = link.len() > 100;
        if split.is_none() || long_link {
            let mut pax = Vec::new();
            if split.is_none() {
                pax.extend(Self::pax_record("path", path));
            }
            if long_link {
                pax.extend(Self::pax_record("linkpath", link));
            }
            let h = Self::header(b"PaxHeader", b"", 0o644, pax.len() as u64, b'x', b"");
            self.out.extend_from_slice(&h);
            self.out.extend_from_slice(&pax);
            self.pad(pax.len() as u64);
        }
        let (prefix, name) = match split {
            Some(s) => s,
            // The PAX record carries the path; the header holds its last 100 bytes.
            None => (&[][..], &path[path.len().saturating_sub(100)..]),
        };
        let link = if long_link { &link[..100] } else { link };
        let h = Self::header(name, prefix, mode, data.len() as u64, kind, link);
        self.out.extend_from_slice(&h);
        self.out.extend_from_slice(data);
        self.pad(data.len() as u64);
    }

    pub fn dir(&mut self, path: &[u8]) {
        let mut p = path.to_vec();
        p.push(b'/');
        self.entry(&p, 0o755, b'5', b"", b"");
    }

    pub fn file(&mut self, path: &[u8], mode: u32, data: &[u8]) {
        self.entry(path, mode, b'0', b"", data);
    }

    pub fn symlink(&mut self, path: &[u8], target: &[u8]) {
        self.entry(path, 0o777, b'2', target, b"");
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.out.extend(std::iter::repeat_n(0u8, 1024));
        self.out
    }
}

/// The new layer: every output entry under `app/`, with the folders above each entry, sorted
/// by path bytes (a folder comes before what it holds). The walk up from an entry stops at the
/// first folder that is already there (every name in the map has all its folders), so an
/// output whose folders are entries (the reader adds them) costs one lookup per entry.
///
/// The uncompressed layer is at most `cap` bytes (`RUUM_BUILD_OUTPUT_MAX`, as the README
/// states): the tar is refused as soon as it grows past it, so headers of many small entries
/// cannot make a layer far larger than the output (Kimi, run 12).
pub fn build_layer(entries: &[OutEntry], cap: u64) -> Result<Layer, String> {
    let mut all: BTreeMap<Vec<u8>, Option<&OutEntry>> = BTreeMap::new();
    all.insert(b"app".to_vec(), None);
    for e in entries {
        let mut full = b"app/".to_vec();
        full.extend_from_slice(&e.path);
        // The folders above it, from the nearest one up to the first one already there.
        let mut end = full.len();
        while let Some(i) = full[..end].iter().rposition(|b| *b == b'/') {
            if all.contains_key(&full[..i]) {
                break;
            }
            all.insert(full[..i].to_vec(), None);
            end = i;
        }
        all.insert(full, Some(e));
    }
    let mut w = TarWriter::new();
    for (path, e) in &all {
        match e.map(|e| &e.kind) {
            None | Some(Kind::Dir) => w.dir(path),
            Some(Kind::File(data)) => w.file(path, e.map_or(0o644, |e| e.mode), data),
            Some(Kind::Symlink(target)) => w.symlink(path, target),
        }
        // Checked after each entry: the buffer passes the cap by at most one entry.
        if w.out.len() as u64 > cap {
            return Err(format!(
                "the new layer is over the output cap of {cap} bytes"
            ));
        }
    }
    let tar = w.finish();
    let diff_id = OciDigest::of(&tar);
    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    // Writing to a Vec cannot fail.
    let _ = gz.write_all(&tar);
    drop(tar);
    let gz = gz.finish().unwrap_or_default();
    Ok(Layer {
        digest: OciDigest::of(&gz),
        diff_id,
        gz: Bytes::from(gz),
    })
}

/// The new image config from the base image's config (see the module docs).
pub fn image_config(
    base: &[u8],
    layer: &Layer,
    start: &str,
    labels: &BTreeMap<String, String>,
) -> Result<Bytes, RuntimeError> {
    let bad = |what: &str| RuntimeError::Internal {
        reason: format!("the base image config: {what}"),
    };
    let mut cfg: Value = serde_json::from_slice(base).map_err(|_| bad("not JSON"))?;
    let obj = cfg.as_object_mut().ok_or_else(|| bad("not an object"))?;
    let base_env = obj
        .get("config")
        .and_then(|c| c.get("Env"))
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    obj.insert(
        "config".to_owned(),
        json!({
            "User": "10001",
            "WorkingDir": "/app",
            "Env": base_env,
            "Entrypoint": Value::Null,
            "Cmd": ["/bin/sh", "-c", start],
            "Labels": labels,
        }),
    );
    obj.insert("created".to_owned(), json!("1970-01-01T00:00:00Z"));
    let rootfs = obj
        .get_mut("rootfs")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| bad("no rootfs"))?;
    rootfs
        .get_mut("diff_ids")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| bad("no diff_ids"))?
        .push(json!(layer.diff_id.as_str()));
    let history = obj
        .entry("history")
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(h) = history.as_array_mut() {
        h.push(json!({
            "created": "1970-01-01T00:00:00Z",
            "created_by": "ruum node22 builder: npm ci, the output under /app",
        }));
    }
    serde_json::to_vec(&cfg)
        .map(Bytes::from)
        .map_err(|_| bad("not serializable"))
}

/// The new image manifest: the base layers, then the new one.
pub fn image_manifest(base_layers: &[Descriptor], config: &Bytes, layer: &Layer) -> Bytes {
    let mut layers: Vec<Value> = base_layers
        .iter()
        .map(|d| json!({"mediaType": d.media_type, "digest": d.digest, "size": d.size}))
        .collect();
    layers.push(json!({
        "mediaType": LAYER_MEDIA,
        "digest": layer.digest.as_str(),
        "size": layer.gz.len(),
    }));
    let m = json!({
        "schemaVersion": 2,
        "mediaType": super::registry::OCI_MANIFEST,
        "config": {
            "mediaType": CONFIG_MEDIA,
            "digest": OciDigest::of(config).as_str(),
            "size": config.len(),
        },
        "layers": layers,
    });
    Bytes::from(serde_json::to_vec(&m).unwrap_or_default())
}

/// The hex SHA-256 of bytes.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// The uncompressed tar of a layer, checked against its diff ID.
    fn tar_of(l: &Layer) -> Bytes {
        let mut t = Vec::new();
        flate2::read::GzDecoder::new(&l.gz[..])
            .read_to_end(&mut t)
            .expect("gunzip");
        assert_eq!(OciDigest::of(&t), l.diff_id);
        Bytes::from(t)
    }

    fn entry(path: &str, kind: Kind) -> OutEntry {
        OutEntry {
            path: path.as_bytes().to_vec(),
            mode: 0o644,
            kind,
        }
    }

    #[test]
    fn the_layer_is_deterministic_and_sorted() {
        let a = vec![
            entry("server.js", Kind::File(Bytes::from_static(b"x"))),
            entry(
                "node_modules/ms/index.js",
                Kind::File(Bytes::from_static(b"y")),
            ),
        ];
        let mut b = a.clone();
        b.reverse();
        let la = build_layer(&a, u64::MAX).expect("layer");
        let lb = build_layer(&b, u64::MAX).expect("layer");
        assert_eq!(la.digest, lb.digest, "the same output gives the same layer");
        assert_eq!(la.diff_id, lb.diff_id);
        let names = super::super::output::read_output(tar_of(&la), u64::MAX)
            .expect("the layer reads back")
            .entries
            .into_iter()
            .map(|e| String::from_utf8(e.path).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "app",
                "app/node_modules",
                "app/node_modules/ms",
                "app/node_modules/ms/index.js",
                "app/server.js"
            ]
        );
    }

    /// Many empty entries: the layer's headers alone pass the output cap, and the layer is
    /// refused instead of growing to many times the output (Kimi, run 12).
    #[test]
    fn the_layer_is_held_to_the_output_cap() {
        let entries: Vec<OutEntry> = (0..2_000)
            .map(|i| entry(&format!("f{i:04}"), Kind::File(Bytes::new())))
            .collect();
        // 2,001 headers of 512 bytes, plus the end marker: about 1 MiB.
        let l = build_layer(&entries, 2 * 1024 * 1024).expect("under the cap");
        assert_eq!(tar_of(&l).len(), 2_001 * 512 + 1024);
        let Err(e) = build_layer(&entries, 512 * 1024) else {
            panic!("a layer over the cap was built");
        };
        assert!(e.contains("over the output cap"), "{e}");
    }

    #[test]
    fn long_paths_and_links_use_pax_records() {
        let long = format!("{}/{}", "d".repeat(150), "f".repeat(120));
        let target = "t".repeat(130);
        let l = build_layer(
            &[
                entry(&long, Kind::File(Bytes::from_static(b"z"))),
                entry("link", Kind::Symlink(target.as_bytes().to_vec())),
            ],
            u64::MAX,
        )
        .expect("layer");
        let back = super::super::output::read_output(tar_of(&l), u64::MAX).expect("reads");
        assert!(
            back.entries
                .iter()
                .any(|e| e.path == format!("app/{long}").into_bytes())
        );
        assert!(
            back.entries
                .iter()
                .any(|e| matches!(&e.kind, Kind::Symlink(t) if *t == target.as_bytes()))
        );
    }

    #[test]
    fn the_config_has_no_volume_no_healthcheck_and_user_10001() {
        let base = br#"{"architecture":"arm64","os":"linux","config":{"Env":["PATH=/usr/bin"],"Entrypoint":["docker-entrypoint.sh"],"Cmd":["node"],"Volumes":{"/data":{}},"Healthcheck":{"Test":["CMD","x"]},"OnBuild":["RUN x"]},"rootfs":{"type":"layers","diff_ids":["sha256:aa"]},"history":[]}"#;
        let l = build_layer(&[], u64::MAX).expect("layer");
        let c = image_config(base, &l, "node server.js", &BTreeMap::new()).expect("config");
        let v: Value = serde_json::from_slice(&c).unwrap();
        let cfg = &v["config"];
        assert_eq!(cfg["User"], "10001");
        assert_eq!(cfg["WorkingDir"], "/app");
        assert_eq!(cfg["Env"], json!(["PATH=/usr/bin"]));
        assert_eq!(cfg["Cmd"], json!(["/bin/sh", "-c", "node server.js"]));
        assert!(cfg.get("Volumes").is_none());
        assert!(cfg.get("Healthcheck").is_none());
        assert!(cfg.get("OnBuild").is_none());
        assert_eq!(v["rootfs"]["diff_ids"][1], l.diff_id.as_str());
    }

    #[test]
    fn the_platform_is_chosen_from_the_index() {
        let idx = include_bytes!("node22-index.json");
        assert_eq!(
            OciDigest::of(idx).as_str(),
            super::super::NODE22_BASE_INDEX,
            "the embedded index is the pinned one"
        );
        let arm = select_manifest(idx, "arm64").expect("arm64");
        let amd = select_manifest(idx, "amd64").expect("amd64");
        assert_ne!(arm.digest, amd.digest);
        assert!(select_manifest(idx, "riscv64").is_err());
    }
}
