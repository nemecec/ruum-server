//! Unpacking a deployment archive under hard limits (card W-2s; decision 0006 H; STEP3 card W-2).
//!
//! The archive is untrusted. [`unpack`] reads a `.tar.gz`/`.tgz`, a `.tar` or a `.zip` from
//! memory into memory: it never writes a file, never follows a link and never calls a library
//! function that unpacks to disk (S-38). The readers are written here (a small ustar reader and
//! a zip reader over the central directory); `flate2` only inflates.
//!
//! Refused with a direct `422 validation_failed` ([`Refusal::Hostile`]): an entry whose name
//! has a `..` segment, an empty or `.` segment inside the path, starts with `/`, holds a `\`
//! (APPNOTE 4.4.17 allows only `/`; in a tar name it is a Windows separator), starts with a drive
//! letter (`C:`), or holds a control byte or bad UTF-8; UNC forms (`\\server\share`,
//! `//server/share`) fall under these rules. Also symbolic and hard links, device files, FIFOs
//! and other special entries, tar extensions other than long names, encrypted or zip64
//! entries, a zip whose local header and central directory disagree, overlapping zip entries,
//! a size that the data does not match, a bad checksum, and an entry name given twice (`app.yaml`
//! twice among them). A leading `./` is dropped (archives made with `tar -C dir .`), and
//! directory entries are checked and skipped.
//!
//! Refused with a direct `413 payload_too_large` ([`Refusal::Limit`]): more files than the
//! plan's `max_archive_files`, more unpacked bytes than the plan's archive limit, an entry over
//! [`Limits::max_entry_bytes`], or a compression ratio over [`Limits::max_ratio`] beyond the
//! first [`RATIO_FREE_BYTES`]. Sizes written in the archive are never trusted: a declared size is
//! checked against the limits before anything is read, and the bytes are counted while they
//! are read.

use super::plan::AppPlanLimits;
use crate::error::{ApiError, Error, ErrorCode};
use bytes::Bytes;
use std::collections::HashSet;
use std::io::Read;

/// The unpacked bytes that are never checked against the compression ratio (small, very
/// compressible files are normal).
pub const RATIO_FREE_BYTES: u64 = 1024 * 1024;

/// The longest entry name, in bytes.
pub const MAX_NAME_BYTES: usize = 1024;

/// The limits of one unpack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// All unpacked bytes together (the plan's archive limit).
    pub max_total_bytes: u64,
    /// One entry.
    pub max_entry_bytes: u64,
    /// Entries (files, folders and other headers) in the archive (TK-1).
    pub max_files: u32,
    /// Unpacked bytes per compressed byte.
    pub max_ratio: u64,
}

impl Limits {
    /// The limits of a plan.
    pub fn of(plan: &AppPlanLimits) -> Self {
        let total = plan.max_archive_bytes.max(0) as u64;
        Self {
            max_total_bytes: total,
            max_entry_bytes: total.min(64 * 1024 * 1024),
            max_files: plan.max_archive_files,
            max_ratio: 200,
        }
    }
}

/// One file of an unpacked archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The checked path: relative, `/` separators, no `.` or `..` segments.
    pub name: String,
    pub data: Bytes,
}

/// The files of an archive, in archive order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unpacked {
    pub entries: Vec<Entry>,
}

impl Unpacked {
    /// The file with this path.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// The paths of the files (the entry list that `archive_glob` fields match, P-66).
    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// All unpacked bytes.
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.data.len() as u64).sum()
    }
}

/// Why an archive is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A hostile or broken entry (`422`). `entry` is the name as written (shown only to the
    /// deployer, who has `edit`; control characters are escaped).
    Hostile { entry: String, reason: String },
    /// The archive itself is not readable (`422`).
    Corrupt { reason: String },
    /// A size or count limit (`413`).
    Limit { what: &'static str, limit: u64 },
}

impl Refusal {
    fn hostile(entry: &str, reason: impl Into<String>) -> Self {
        Refusal::Hostile {
            entry: entry.to_string(),
            reason: reason.into(),
        }
    }

    fn corrupt(reason: impl Into<String>) -> Self {
        Refusal::Corrupt {
            reason: reason.into(),
        }
    }

    /// The API error of the refusal (`error.details.stage` is `unpack`).
    pub fn into_error(self) -> Error {
        match self {
            Refusal::Hostile { entry, reason } => {
                let shown: String = entry.escape_debug().take(200).collect();
                Error::Api(
                    ApiError::new(
                        ErrorCode::ValidationFailed,
                        format!("the archive entry \"{shown}\" is refused: {reason}"),
                    )
                    .with_hint("archive_entry_refused")
                    .with_details(serde_json::json!({ "stage": "unpack", "entry": shown })),
                )
            }
            Refusal::Corrupt { reason } => Error::Api(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    format!("the archive cannot be read: {reason}"),
                )
                .with_hint("deploy a .tar.gz, .tgz, .tar or .zip with app.yaml at its root")
                .with_details(serde_json::json!({ "stage": "unpack" })),
            ),
            Refusal::Limit { what, limit } => Error::Api(
                ApiError::new(
                    ErrorCode::PayloadTooLarge,
                    format!("the archive is over the limit of {what} ({limit})"),
                )
                .with_hint("deploy a smaller archive, or move to a larger plan")
                .with_details(serde_json::json!({ "stage": "unpack", "limit": what })),
            ),
        }
    }
}

type R<T> = Result<T, Refusal>;

/// The format of an archive, by its first bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    TarGz,
    Tar,
    Zip,
}

/// The format of `bytes`, or `None`.
pub fn format_of(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        Some(Format::TarGz)
    } else if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        Some(Format::Zip)
    } else if bytes.len() >= 512 && &bytes[257..262] == b"ustar" {
        Some(Format::Tar)
    } else {
        None
    }
}

/// Unpacks `bytes` under `limits` (see the module docs).
pub fn unpack(bytes: &[u8], limits: &Limits) -> R<Unpacked> {
    if bytes.len() as u64 > limits.max_total_bytes {
        return Err(Refusal::Limit {
            what: "archive bytes",
            limit: limits.max_total_bytes,
        });
    }
    match format_of(bytes) {
        Some(Format::TarGz) => {
            let gz = flate2::read::GzDecoder::new(bytes);
            let counted = Counted {
                inner: gz,
                read: 0,
                // Headers and padding come on top of the file bytes.
                max: limits
                    .max_total_bytes
                    .saturating_add(limits.max_total_bytes / 8 + 4096),
                compressed: bytes.len() as u64,
                ratio: limits.max_ratio,
            };
            read_tar(counted, limits)
        }
        Some(Format::Tar) => read_tar(bytes, limits),
        Some(Format::Zip) => read_zip(bytes, limits),
        None => Err(Refusal::corrupt(
            "it is not a .tar.gz, .tar or .zip archive",
        )),
    }
}

/// A reader that counts the bytes it gives and refuses more than `max` or a ratio over
/// `ratio` against `compressed`.
struct Counted<T> {
    inner: T,
    read: u64,
    max: u64,
    compressed: u64,
    ratio: u64,
}

const LIMIT_MARK: &str = "ruum-archive-limit";
const RATIO_MARK: &str = "ruum-archive-ratio";

impl<T: Read> Read for Counted<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        if self.read > self.max {
            return Err(std::io::Error::other(LIMIT_MARK));
        }
        if self.read > RATIO_FREE_BYTES && self.read > self.compressed.saturating_mul(self.ratio) {
            return Err(std::io::Error::other(RATIO_MARK));
        }
        Ok(n)
    }
}

fn io_refusal(e: std::io::Error, limits: &Limits) -> Refusal {
    match e.to_string().as_str() {
        LIMIT_MARK => Refusal::Limit {
            what: "unpacked bytes",
            limit: limits.max_total_bytes,
        },
        RATIO_MARK => Refusal::Limit {
            what: "compression ratio",
            limit: limits.max_ratio,
        },
        _ if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            Refusal::corrupt("the archive ends in the middle of an entry")
        }
        _ => Refusal::corrupt("the compressed data is not valid"),
    }
}

/// Checks an entry name. `Ok(None)` for a folder entry (checked, then skipped).
pub fn check_name(raw: &str) -> R<Option<String>> {
    if raw.is_empty() {
        return Err(Refusal::hostile(raw, "an empty name"));
    }
    if raw.len() > MAX_NAME_BYTES {
        return Err(Refusal::hostile(raw, "the name is too long"));
    }
    if raw.contains('\\') {
        return Err(Refusal::hostile(
            raw,
            "a \\ in the name (a Windows path separator)",
        ));
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err(Refusal::hostile(raw, "a control character in the name"));
    }
    if raw.starts_with('/') {
        return Err(Refusal::hostile(raw, "an absolute path"));
    }
    let mut rest = raw;
    while let Some(r) = rest.strip_prefix("./") {
        rest = r;
    }
    // After the leading `./` parts, so `./C:/x` is refused like `C:/x`, and `.//x` like `/x`
    // (review W-2s, Kimi 6).
    if rest.starts_with('/') {
        return Err(Refusal::hostile(raw, "an absolute path"));
    }
    let b = rest.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Err(Refusal::hostile(raw, "a drive letter"));
    }
    let folder = rest.ends_with('/') || rest.is_empty();
    let body = rest.strip_suffix('/').unwrap_or(rest);
    if body.is_empty() {
        // `./` or the like: the archive root.
        return Ok(None);
    }
    for seg in body.split('/') {
        if seg.is_empty() {
            return Err(Refusal::hostile(raw, "an empty path segment"));
        }
        if seg == ".." {
            return Err(Refusal::hostile(raw, "a .. segment"));
        }
        if seg == "." {
            return Err(Refusal::hostile(raw, "a . segment"));
        }
    }
    if folder {
        Ok(None)
    } else {
        Ok(Some(body.to_string()))
    }
}

/// Collects files and enforces the count and total limits and unique names.
struct Collector<'l> {
    limits: &'l Limits,
    entries: Vec<Entry>,
    names: HashSet<String>,
    count: u64,
    total: u64,
}

impl<'l> Collector<'l> {
    fn new(limits: &'l Limits) -> Self {
        Self {
            limits,
            entries: Vec::new(),
            names: HashSet::new(),
            count: 0,
            total: 0,
        }
    }

    /// Counts one entry of any kind.
    fn count(&mut self) -> R<()> {
        self.count += 1;
        if self.count > u64::from(self.limits.max_files) {
            return Err(Refusal::Limit {
                what: "files",
                limit: u64::from(self.limits.max_files),
            });
        }
        Ok(())
    }

    /// Checks a declared size before reading it.
    fn reserve(&mut self, size: u64) -> R<()> {
        if size > self.limits.max_entry_bytes {
            return Err(Refusal::Limit {
                what: "bytes of one file",
                limit: self.limits.max_entry_bytes,
            });
        }
        self.total = self.total.saturating_add(size);
        if self.total > self.limits.max_total_bytes {
            return Err(Refusal::Limit {
                what: "unpacked bytes",
                limit: self.limits.max_total_bytes,
            });
        }
        Ok(())
    }

    fn claim(&mut self, raw: &str, name: &str) -> R<()> {
        if !self.names.insert(name.to_string()) {
            return Err(Refusal::hostile(raw, "the name is given twice"));
        }
        Ok(())
    }

    fn push(&mut self, name: String, data: Vec<u8>) {
        self.entries.push(Entry {
            name,
            data: Bytes::from(data),
        });
    }
}

// ---------------------------------------------------------------------------- tar

fn tar_str(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

fn octal(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        // Base-256 sizes are for files over 8 GiB: never needed here.
        return None;
    }
    let s = std::str::from_utf8(tar_str(field)).ok()?.trim();
    if s.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(s, 8).ok()
}

fn read_exact_vec<T: Read>(r: &mut T, n: u64, limits: &Limits) -> R<Vec<u8>> {
    let mut out = Vec::with_capacity(n.min(1 << 20) as usize);
    let got = r
        .by_ref()
        .take(n)
        .read_to_end(&mut out)
        .map_err(|e| io_refusal(e, limits))?;
    if got as u64 != n {
        return Err(Refusal::corrupt(
            "the archive ends in the middle of an entry",
        ));
    }
    Ok(out)
}

fn skip<T: Read>(r: &mut T, n: u64, limits: &Limits) -> R<()> {
    let copied = std::io::copy(&mut r.by_ref().take(n), &mut std::io::sink())
        .map_err(|e| io_refusal(e, limits))?;
    if copied != n {
        return Err(Refusal::corrupt(
            "the archive ends in the middle of an entry",
        ));
    }
    Ok(())
}

fn padding(size: u64) -> u64 {
    (512 - size % 512) % 512
}

/// The `path` record of a pax extended header (`<len> <key>=<value>\n` records). `size` and
/// other keys that change how the archive is read are refused.
fn pax_path(data: &[u8]) -> R<Option<String>> {
    let mut rest = data;
    let mut path = None;
    while !rest.is_empty() {
        let sp = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| Refusal::corrupt("a bad pax record"))?;
        let len: usize = std::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Refusal::corrupt("a bad pax record"))?;
        if len <= sp + 1 || len > rest.len() || rest[len - 1] != b'\n' {
            return Err(Refusal::corrupt("a bad pax record"));
        }
        let rec = &rest[sp + 1..len - 1];
        let eq = rec
            .iter()
            .position(|&b| b == b'=')
            .ok_or_else(|| Refusal::corrupt("a bad pax record"))?;
        let (key, value) = (&rec[..eq], &rec[eq + 1..]);
        match key {
            b"path" => {
                let v = std::str::from_utf8(value)
                    .map_err(|_| Refusal::hostile("(pax path)", "a name that is not UTF-8"))?;
                path = Some(v.to_string());
            }
            b"size"
            | b"linkpath"
            | b"GNU.sparse.name"
            | b"GNU.sparse.map"
            | b"GNU.sparse.size"
            | b"GNU.sparse.realsize"
            | b"GNU.sparse.major" => {
                return Err(Refusal::hostile(
                    "(pax header)",
                    "a pax record that changes how the archive is read",
                ));
            }
            _ => {}
        }
        rest = &rest[len..];
    }
    Ok(path)
}

fn read_tar<T: Read>(mut r: T, limits: &Limits) -> R<Unpacked> {
    let mut c = Collector::new(limits);
    let mut long_name: Option<String> = None;
    loop {
        let mut h = [0u8; 512];
        match r.read_exact(&mut h) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(Refusal::corrupt("the tar archive has no end blocks"));
            }
            Err(e) => return Err(io_refusal(e, limits)),
        }
        if h.iter().all(|&b| b == 0) {
            // The end of the archive (the rest is padding).
            break;
        }
        let stored = octal(&h[148..156]).ok_or_else(|| Refusal::corrupt("a bad tar checksum"))?;
        let sum: u64 = h
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(b)
                }
            })
            .sum();
        if sum != stored {
            return Err(Refusal::corrupt("a bad tar checksum"));
        }
        c.count()?;
        let size = octal(&h[124..136]).ok_or_else(|| Refusal::corrupt("a bad tar size"))?;
        let flag = h[156];
        let ustar = &h[257..262] == b"ustar";
        let header_name = {
            let name = tar_str(&h[0..100]);
            let prefix = if ustar {
                tar_str(&h[345..500])
            } else {
                &[][..]
            };
            let mut full = Vec::with_capacity(prefix.len() + 1 + name.len());
            if !prefix.is_empty() {
                full.extend_from_slice(prefix);
                full.push(b'/');
            }
            full.extend_from_slice(name);
            full
        };
        let raw_name = match long_name.take() {
            Some(n) => n,
            None => String::from_utf8(header_name)
                .map_err(|_| Refusal::hostile("(not UTF-8)", "a name that is not UTF-8"))?,
        };
        match flag {
            // GNU long name: the name of the next entry.
            b'L' => {
                if size > MAX_NAME_BYTES as u64 {
                    return Err(Refusal::hostile("(long name)", "the name is too long"));
                }
                let data = read_exact_vec(&mut r, size, limits)?;
                skip(&mut r, padding(size), limits)?;
                let name = String::from_utf8(tar_str(&data).to_vec())
                    .map_err(|_| Refusal::hostile("(long name)", "a name that is not UTF-8"))?;
                long_name = Some(name);
            }
            // A pax header of the next entry; only `path` is used.
            b'x' => {
                if size > 64 * 1024 {
                    return Err(Refusal::hostile(
                        "(pax header)",
                        "the pax header is too long",
                    ));
                }
                let data = read_exact_vec(&mut r, size, limits)?;
                skip(&mut r, padding(size), limits)?;
                long_name = pax_path(&data)?;
            }
            // A global pax header: nothing of it is used, but it may not change sizes.
            b'g' => {
                if size > 64 * 1024 {
                    return Err(Refusal::hostile(
                        "(pax header)",
                        "the pax header is too long",
                    ));
                }
                let data = read_exact_vec(&mut r, size, limits)?;
                skip(&mut r, padding(size), limits)?;
                pax_path(&data)?;
            }
            b'0' | 0 | b'7' => {
                let Some(name) = check_name(&raw_name)? else {
                    return Err(Refusal::hostile(
                        &raw_name,
                        "a file entry with a folder name",
                    ));
                };
                c.claim(&raw_name, &name)?;
                c.reserve(size)?;
                let data = read_exact_vec(&mut r, size, limits)?;
                skip(&mut r, padding(size), limits)?;
                c.push(name, data);
            }
            b'5' => {
                check_name(&raw_name)?;
                if size != 0 {
                    return Err(Refusal::hostile(&raw_name, "a folder entry with data"));
                }
            }
            b'1' => return Err(Refusal::hostile(&raw_name, "a hard link")),
            b'2' => return Err(Refusal::hostile(&raw_name, "a symbolic link")),
            b'3' => return Err(Refusal::hostile(&raw_name, "a character device")),
            b'4' => return Err(Refusal::hostile(&raw_name, "a block device")),
            b'6' => return Err(Refusal::hostile(&raw_name, "a FIFO")),
            b'K' => return Err(Refusal::hostile(&raw_name, "a link (long link name)")),
            _ => {
                return Err(Refusal::hostile(
                    &raw_name,
                    "an entry type that is not a file",
                ));
            }
        }
    }
    if long_name.is_some() {
        return Err(Refusal::corrupt("a long name without an entry"));
    }
    Ok(Unpacked { entries: c.entries })
}

// ---------------------------------------------------------------------------- zip

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    b.get(i..i + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

struct CentralEntry {
    raw_name: String,
    name_bytes: Vec<u8>,
    flags: u16,
    method: u16,
    crc: u32,
    csize: u32,
    usize: u32,
    offset: u32,
    unix_mode: Option<u32>,
}

/// The extra field holds a zip64 record.
fn has_zip64(extra: &[u8]) -> bool {
    let mut i = 0;
    while i + 4 <= extra.len() {
        let id = u16::from_le_bytes([extra[i], extra[i + 1]]);
        let len = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
        if id == 0x0001 {
            return true;
        }
        i += 4 + len;
    }
    false
}

fn read_zip(b: &[u8], limits: &Limits) -> R<Unpacked> {
    let bad = |what: &str| Refusal::corrupt(format!("the zip archive is not valid: {what}"));
    // The end of central directory record: within the last 22 + 65535 bytes, and the comment
    // must end the file exactly.
    if b.len() < 22 {
        return Err(bad("too short"));
    }
    let from = b.len().saturating_sub(22 + 65_535);
    let eocd = (from..=b.len() - 22)
        .rev()
        .find(|&i| {
            u32_at(b, i) == Some(0x0605_4b50)
                && u16_at(b, i + 20).is_some_and(|c| i + 22 + c as usize == b.len())
        })
        .ok_or_else(|| bad("no end of central directory"))?;
    if eocd >= 20 && u32_at(b, eocd - 20) == Some(0x0706_4b50) {
        return Err(Refusal::hostile(
            "(zip64)",
            "zip64 archives are not accepted",
        ));
    }
    let disk = u16_at(b, eocd + 4).unwrap_or(1);
    let cd_disk = u16_at(b, eocd + 6).unwrap_or(1);
    let n_here = u16_at(b, eocd + 8).unwrap_or(0);
    let n_total = u16_at(b, eocd + 10).unwrap_or(0);
    let cd_size = u32_at(b, eocd + 12).unwrap_or(0);
    let cd_offset = u32_at(b, eocd + 16).unwrap_or(0);
    if n_total == 0xffff || cd_size == 0xffff_ffff || cd_offset == 0xffff_ffff {
        return Err(Refusal::hostile(
            "(zip64)",
            "zip64 archives are not accepted",
        ));
    }
    if disk != 0 || cd_disk != 0 || n_here != n_total {
        return Err(bad("split archives are not accepted"));
    }
    if u64::from(n_total) > u64::from(limits.max_files) {
        return Err(Refusal::Limit {
            what: "files",
            limit: u64::from(limits.max_files),
        });
    }
    let cd_start = cd_offset as usize;
    let cd_end = cd_start
        .checked_add(cd_size as usize)
        .filter(|&e| e <= eocd)
        .ok_or_else(|| bad("the central directory is out of place"))?;
    let mut central = Vec::with_capacity(n_total as usize);
    let mut i = cd_start;
    for _ in 0..n_total {
        if u32_at(b, i) != Some(0x0201_4b50) {
            return Err(bad("a central directory entry"));
        }
        let field = |o: usize| u16_at(b, i + o).ok_or_else(|| bad("short central entry"));
        let field32 = |o: usize| u32_at(b, i + o).ok_or_else(|| bad("short central entry"));
        let made_by = field(4)?;
        let flags = field(8)?;
        let method = field(10)?;
        let crc = field32(16)?;
        let csize = field32(20)?;
        let usize = field32(24)?;
        let nlen = field(28)? as usize;
        let elen = field(30)? as usize;
        let clen = field(32)? as usize;
        let start_disk = field(34)?;
        let attrs = field32(38)?;
        let offset = field32(42)?;
        let name_bytes = b
            .get(i + 46..i + 46 + nlen)
            .ok_or_else(|| bad("short central entry"))?
            .to_vec();
        let extra = b
            .get(i + 46 + nlen..i + 46 + nlen + elen)
            .ok_or_else(|| bad("short central entry"))?;
        let raw_name = String::from_utf8(name_bytes.clone())
            .map_err(|_| Refusal::hostile("(not UTF-8)", "a name that is not UTF-8"))?;
        if csize == 0xffff_ffff || usize == 0xffff_ffff || offset == 0xffff_ffff || has_zip64(extra)
        {
            return Err(Refusal::hostile(
                &raw_name,
                "zip64 entries are not accepted",
            ));
        }
        if start_disk != 0 {
            return Err(bad("split archives are not accepted"));
        }
        let unix_mode = (made_by >> 8 == 3).then_some(attrs >> 16);
        central.push(CentralEntry {
            raw_name,
            name_bytes,
            flags,
            method,
            crc,
            csize,
            usize,
            offset,
            unix_mode,
        });
        i += 46 + nlen + elen + clen;
        if i > cd_end {
            return Err(bad("the central directory is longer than it says"));
        }
    }
    let mut c = Collector::new(limits);
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(central.len());
    for e in &central {
        c.count()?;
        if let Some(mode) = e.unix_mode {
            match mode & 0o170_000 {
                0 | 0o100_000 | 0o040_000 => {}
                0o120_000 => return Err(Refusal::hostile(&e.raw_name, "a symbolic link")),
                0o020_000 => return Err(Refusal::hostile(&e.raw_name, "a character device")),
                0o060_000 => return Err(Refusal::hostile(&e.raw_name, "a block device")),
                0o010_000 => return Err(Refusal::hostile(&e.raw_name, "a FIFO")),
                _ => {
                    return Err(Refusal::hostile(&e.raw_name, "an entry that is not a file"));
                }
            }
        }
        let name = check_name(&e.raw_name)?;
        if e.flags & 0x0001 != 0 || e.flags & 0x0040 != 0 {
            return Err(Refusal::hostile(&e.raw_name, "an encrypted entry"));
        }
        if e.method != 0 && e.method != 8 {
            return Err(Refusal::hostile(
                &e.raw_name,
                "a compression method other than stored or deflate",
            ));
        }
        // The local header must agree with the central directory.
        let lo = e.offset as usize;
        if u32_at(b, lo) != Some(0x0403_4b50) {
            return Err(Refusal::hostile(
                &e.raw_name,
                "no local header at its offset",
            ));
        }
        let lfield = |o: usize| u16_at(b, lo + o).ok_or_else(|| bad("short local header"));
        let lfield32 = |o: usize| u32_at(b, lo + o).ok_or_else(|| bad("short local header"));
        let lflags = lfield(6)?;
        let lmethod = lfield(8)?;
        let lnlen = lfield(26)? as usize;
        let lelen = lfield(28)? as usize;
        let lname = b
            .get(lo + 30..lo + 30 + lnlen)
            .ok_or_else(|| bad("short local header"))?;
        let lextra = b
            .get(lo + 30 + lnlen..lo + 30 + lnlen + lelen)
            .ok_or_else(|| bad("short local header"))?;
        let disagree = lname != e.name_bytes.as_slice()
            || lmethod != e.method
            || (lflags & 0x0001) != (e.flags & 0x0001)
            || has_zip64(lextra)
            || (lflags & 0x0008 == 0
                && (lfield32(14)? != e.crc
                    || lfield32(18)? != e.csize
                    || lfield32(22)? != e.usize));
        if disagree {
            return Err(Refusal::hostile(
                &e.raw_name,
                "the local header and the central directory disagree",
            ));
        }
        let data_start = lo + 30 + lnlen + lelen;
        let data_end = data_start
            .checked_add(e.csize as usize)
            .filter(|&end| end <= cd_start)
            .ok_or_else(|| Refusal::hostile(&e.raw_name, "the data runs past its place"))?;
        spans.push((lo, data_end));
        let Some(name) = name else {
            // A folder entry: checked above, nothing to read.
            if e.usize != 0 {
                return Err(Refusal::hostile(&e.raw_name, "a folder entry with data"));
            }
            continue;
        };
        c.claim(&e.raw_name, &name)?;
        c.reserve(u64::from(e.usize))?;
        let compressed = &b[data_start..data_end];
        let size = u64::from(e.usize);
        if size > RATIO_FREE_BYTES && size > u64::from(e.csize).saturating_mul(limits.max_ratio) {
            return Err(Refusal::Limit {
                what: "compression ratio",
                limit: limits.max_ratio,
            });
        }
        let data = match e.method {
            0 => compressed.to_vec(),
            _ => {
                let mut out = Vec::with_capacity(size as usize);
                flate2::read::DeflateDecoder::new(compressed)
                    .take(size + 1)
                    .read_to_end(&mut out)
                    .map_err(|_| Refusal::hostile(&e.raw_name, "the deflate data is not valid"))?;
                out
            }
        };
        if data.len() as u64 != size {
            return Err(Refusal::hostile(
                &e.raw_name,
                "the data does not have the size the archive gives",
            ));
        }
        let mut crc = flate2::Crc::new();
        crc.update(&data);
        if crc.sum() != e.crc {
            return Err(Refusal::hostile(&e.raw_name, "a bad CRC-32"));
        }
        c.push(name, data);
    }
    // Entries never share bytes (overlapping entries are a known zip bomb).
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[1].0 < w[0].1) {
        return Err(Refusal::hostile("(zip)", "entries overlap"));
    }
    Ok(Unpacked { entries: c.entries })
}

// ---------------------------------------------------------------------------- packing

/// Packs files into a ustar archive (the archive of a folder source): mode 0644, owner 0, time
/// 0, sorted as given. A path over the ustar limits (a name of 100 bytes after a prefix of 155)
/// is refused (`422`).
pub fn pack_tar(files: &[(String, &[u8])]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    for (path, data) in files {
        let mut h = [0u8; 512];
        let (prefix, name) = split_ustar(path).ok_or_else(|| {
            Error::Api(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    format!("the path {path:?} is too long for an archive"),
                )
                .with_hint("use paths of at most 100 characters per file name"),
            )
        })?;
        h[..name.len()].copy_from_slice(name.as_bytes());
        put_octal(&mut h[100..108], 0o644);
        put_octal(&mut h[108..116], 0);
        put_octal(&mut h[116..124], 0);
        put_octal(&mut h[124..136], data.len() as u64);
        put_octal(&mut h[136..148], 0);
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
        let s = format!("{sum:06o}\0 ");
        h[148..156].copy_from_slice(s.as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(data);
        out.extend(std::iter::repeat_n(
            0u8,
            padding(data.len() as u64) as usize,
        ));
    }
    out.extend(std::iter::repeat_n(0u8, 1024));
    Ok(out)
}

fn put_octal(field: &mut [u8], v: u64) {
    let digits = field.len() - 1;
    let s = format!("{v:0digits$o}");
    let s = &s[s.len().saturating_sub(digits)..];
    field[..s.len()].copy_from_slice(s.as_bytes());
    field[digits] = 0;
}

/// The ustar prefix and name of a path.
fn split_ustar(path: &str) -> Option<(&str, &str)> {
    if path.len() <= 100 {
        return Some(("", path));
    }
    for (i, c) in path.char_indices() {
        if c == '/' && i <= 155 && path.len() - i - 1 <= 100 && i > 0 {
            return Some((&path[..i], &path[i + 1..]));
        }
    }
    None
}

#[cfg(test)]
mod tests;
