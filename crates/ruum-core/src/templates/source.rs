//! Where the gallery comes from (decision 0006 point 11): a [`TemplateSource`] loaded once at
//! start, never a node of a tenant, so no template operation reads two tenants.
//!
//! [`DirTemplateSource`] reads a directory (`RUUM_GALLERY_DIR`, default the bundled `gallery/`).
//! The directory is trusted platform content, but the loader is strict, so a self-hoster's
//! mistake fails at start and not in a tenant's call:
//!
//! - every subdirectory with a `template.yaml` is a template; its slug is the directory name and
//!   must match `[a-z0-9][a-z0-9-]{0,62}` (the `TemplateId` pattern); other files and
//!   subdirectories without `template.yaml` (the README, `reference/`) are skipped;
//! - no symlink anywhere in a template (one is refused, never followed), only regular files and
//!   folders, at most [`MAX_FILES`] files, [`MAX_FILES`] folders, [`MAX_DEPTH`] levels and the
//!   plan's archive bytes in all;
//! - `template.yaml` passes [`super::manifest::check`] (the schema, relative paths, issue
//!   schemas);
//! - every path that `setup` names exists inside the template: `folder.from` a folder,
//!   `database.manifest` and `database.sample_data` files, `database.migrations` a folder;
//! - `app.archive` (`<name>.tar.gz`) is packed from the folder `<name>/` ([`pack_tar_gz`]:
//!   sorted names, mode 0644 or 0755, time 0, owner 0 without names), or read as it is when
//!   the file is there; it must pass [`crate::apps::archive::unpack`] within the plan's limits
//!   and its `app.yaml` the rules of [`crate::apps::manifest::validate`];
//! - a database manifest passes [`crate::databases::manifest::check`] and each migration's
//!   `-- ruum:classes` header parses.
//!
//! A failing template stops the start with an error that names the template and the field;
//! with `strict: false` (`RUUM_GALLERY_STRICT=false`) it is skipped with a warning. A missing
//! or empty directory gives an empty gallery and one warning.
//!
//! [`MemoryTemplateSource`] holds templates given in code (unit tests); one conformance suite
//! tests both.

use super::TemplateKind;
use super::manifest;
use crate::apps::archive::{self, Limits};
use crate::apps::deployments::yaml;
use crate::apps::plan::AppPlanLimits;
use bytes::Bytes;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

/// The most files in one template folder.
pub const MAX_FILES: usize = 10_000;
/// The deepest folder level in a template folder.
pub const MAX_DEPTH: usize = 32;
/// The suffix of an app archive in `setup`.
pub const ARCHIVE_SUFFIX: &str = ".tar.gz";

/// One template of the gallery.
#[derive(Clone, Debug, PartialEq)]
pub struct GalleryTemplate {
    /// The directory name; the ID is `gallery:<slug>`.
    pub slug: String,
    /// The parsed and checked `template.yaml`.
    pub manifest: Value,
    /// The files of the template (path relative to the template folder, `/` separators), with
    /// the packed app archives (`app.tar.gz` packed from `app/`). The setup of card L-2 reads
    /// them.
    pub files: BTreeMap<String, Bytes>,
}

impl GalleryTemplate {
    pub fn kind(&self) -> TemplateKind {
        match self.manifest.get("kind").and_then(Value::as_str) {
            Some("issue") => TemplateKind::Issue,
            _ => TemplateKind::Project,
        }
    }

    pub fn name(&self) -> &str {
        self.manifest
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn description(&self) -> &str {
        self.manifest
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    pub fn version(&self) -> &str {
        self.manifest
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// The gallery templates, loaded once.
pub trait TemplateSource: Send + Sync {
    /// Every template, by slug.
    fn templates(&self) -> Vec<Arc<GalleryTemplate>>;
    /// The template with this slug.
    fn get(&self, slug: &str) -> Option<Arc<GalleryTemplate>>;
}

/// Why a gallery template was refused at load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    /// The directory name of the template (or `(gallery)` for the directory itself).
    pub template: String,
    /// The field or path that is wrong (a JSON pointer of `template.yaml`, or a file path).
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gallery template {:?}: {}: {}",
            self.template, self.field, self.message
        )
    }
}

impl std::error::Error for LoadError {}

fn load_error(template: &str, field: impl Into<String>, message: impl Into<String>) -> LoadError {
    LoadError {
        template: template.to_string(),
        field: field.into(),
        message: message.into(),
    }
}

/// How a directory is loaded.
#[derive(Clone, Copy, Debug)]
pub struct LoadOptions {
    /// Refuse the whole gallery when one template fails (the default); else skip it with a
    /// warning.
    pub strict: bool,
    /// The plan whose archive limits an app archive must fit (the smallest plan, so a gallery
    /// app deploys on every plan).
    pub plan: AppPlanLimits,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            strict: true,
            plan: AppPlanLimits::of(crate::tenants::Plan::Free),
        }
    }
}

/// Whether `slug` is a valid gallery slug (`[a-z0-9][a-z0-9-]{0,62}`).
pub fn valid_slug(slug: &str) -> bool {
    let b = slug.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

// ---------------------------------------------------------------------------- memory

/// Templates given in code.
#[derive(Clone, Debug, Default)]
pub struct MemoryTemplateSource {
    by_slug: BTreeMap<String, Arc<GalleryTemplate>>,
}

impl MemoryTemplateSource {
    /// An empty gallery.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The templates, after the slug and manifest checks (not the file checks of the directory
    /// loader).
    pub fn new(templates: Vec<GalleryTemplate>) -> Result<Self, LoadError> {
        let mut by_slug = BTreeMap::new();
        for t in templates {
            if !valid_slug(&t.slug) {
                return Err(load_error(&t.slug, "slug", "the slug breaks the pattern"));
            }
            manifest::check(&t.manifest)
                .map_err(|p| load_error(&t.slug, p.pointer.clone(), p.message))?;
            let slug = t.slug.clone();
            if by_slug.insert(slug.clone(), Arc::new(t)).is_some() {
                return Err(load_error(&slug, "slug", "two templates have this slug"));
            }
        }
        Ok(Self { by_slug })
    }
}

impl TemplateSource for MemoryTemplateSource {
    fn templates(&self) -> Vec<Arc<GalleryTemplate>> {
        self.by_slug.values().cloned().collect()
    }

    fn get(&self, slug: &str) -> Option<Arc<GalleryTemplate>> {
        self.by_slug.get(slug).cloned()
    }
}

// ---------------------------------------------------------------------------- directory

/// The gallery of a directory (see the module docs).
#[derive(Clone, Debug)]
pub struct DirTemplateSource {
    inner: MemoryTemplateSource,
}

impl TemplateSource for DirTemplateSource {
    fn templates(&self) -> Vec<Arc<GalleryTemplate>> {
        self.inner.templates()
    }

    fn get(&self, slug: &str) -> Option<Arc<GalleryTemplate>> {
        self.inner.get(slug)
    }
}

/// The files of one template folder: path → (bytes, executable), and the folders.
#[derive(Default)]
struct Tree {
    files: BTreeMap<String, (Vec<u8>, bool)>,
    dirs: BTreeSet<String>,
    bytes: u64,
}

impl DirTemplateSource {
    /// Loads every template of `dir` (see the module docs).
    pub fn load(dir: &Path, opts: &LoadOptions) -> Result<Self, LoadError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(dir = %dir.display(), "the gallery directory does not exist; the gallery is empty");
                return Ok(Self {
                    inner: MemoryTemplateSource::empty(),
                });
            }
            Err(e) => {
                return Err(load_error(
                    "(gallery)",
                    dir.display().to_string(),
                    format!("cannot read the gallery directory: {e}"),
                ));
            }
        };
        let mut names = Vec::new();
        for e in entries {
            let e =
                e.map_err(|e| load_error("(gallery)", dir.display().to_string(), e.to_string()))?;
            names.push(e.file_name());
        }
        names.sort();
        let mut templates = Vec::new();
        for os in names {
            let name = os.to_string_lossy().to_string();
            let path = dir.join(&os);
            match load_one(&name, &path, opts) {
                Ok(Some(t)) => templates.push(t),
                Ok(None) => {}
                Err(e) if !opts.strict => {
                    tracing::warn!(error = %e, "a gallery template is skipped (RUUM_GALLERY_STRICT=false)");
                }
                Err(e) => return Err(e),
            }
        }
        if templates.is_empty() {
            tracing::warn!(dir = %dir.display(), "the gallery directory has no template; the gallery is empty");
        }
        Ok(Self {
            inner: MemoryTemplateSource::new(templates)?,
        })
    }
}

/// One entry of the gallery directory: `None` when it is not a template.
fn load_one(
    name: &str,
    path: &Path,
    opts: &LoadOptions,
) -> Result<Option<GalleryTemplate>, LoadError> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| load_error(name, "/", format!("cannot read: {e}")))?;
    if meta.file_type().is_symlink() {
        return Err(load_error(
            name,
            "/",
            "a symlink in the gallery directory is refused",
        ));
    }
    if !meta.is_dir() {
        return Ok(None);
    }
    match std::fs::symlink_metadata(path.join("template.yaml")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(load_error(
                name,
                "template.yaml",
                format!("cannot read: {e}"),
            ));
        }
        Ok(_) => {}
    }
    if !valid_slug(name) {
        return Err(load_error(
            name,
            "slug",
            "the directory name must match [a-z0-9][a-z0-9-]{0,62}",
        ));
    }
    let mut tree = Tree::default();
    let max_bytes = (opts.plan.max_archive_bytes.max(0) as u64).saturating_mul(2);
    collect(name, path, "", &mut tree, max_bytes)?;
    let (raw, _) = tree
        .files
        .get("template.yaml")
        .ok_or_else(|| load_error(name, "template.yaml", "not a regular file"))?;
    let m = manifest::parse(raw).map_err(|p| load_error(name, "template.yaml", p.message))?;
    manifest::check(&m).map_err(|p| load_error(name, p.pointer.clone(), p.message))?;
    let mut files: BTreeMap<String, Bytes> = tree
        .files
        .iter()
        .map(|(k, (v, _))| (k.clone(), Bytes::from(v.clone())))
        .collect();
    let steps = m
        .get("setup")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (i, step) in steps.iter().enumerate() {
        if let Some(from) = step
            .get("folder")
            .and_then(|f| f.get("from"))
            .and_then(Value::as_str)
            && !tree.dirs.contains(&manifest::normal_path(from))
        {
            return Err(load_error(
                name,
                format!("/setup/{i}/folder/from"),
                "no such folder in the template",
            ));
        }
        if let Some(d) = step.get("database") {
            check_database(name, i, d, &tree)?;
        }
        if let Some(a) = step.get("app") {
            let at = format!("/setup/{i}/app/archive");
            let archive_path = a.get("archive").and_then(Value::as_str).unwrap_or_default();
            let bytes = match tree.files.get(archive_path) {
                Some((b, _)) => b.clone(),
                None => {
                    let Some(base) = archive_path.strip_suffix(ARCHIVE_SUFFIX) else {
                        return Err(load_error(
                            name,
                            at,
                            "an app archive is a file or a folder <name>/ packed into <name>.tar.gz",
                        ));
                    };
                    let prefix = format!("{base}/");
                    if !tree.dirs.contains(base) {
                        return Err(load_error(
                            name,
                            at,
                            "no such archive file or folder in the template",
                        ));
                    }
                    let mut entries: Vec<(String, bool, &[u8])> = tree
                        .files
                        .iter()
                        .filter_map(|(k, (v, x))| {
                            k.strip_prefix(&prefix)
                                .map(|rel| (rel.to_string(), *x, v.as_slice()))
                        })
                        .collect();
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                    pack_tar_gz(&entries).map_err(|e| load_error(name, at.clone(), e))?
                }
            };
            check_app_archive(&bytes, &opts.plan).map_err(|e| load_error(name, at.clone(), e))?;
            files.insert(archive_path.to_string(), Bytes::from(bytes));
        }
    }
    Ok(Some(GalleryTemplate {
        slug: name.to_string(),
        manifest: m,
        files,
    }))
}

/// Reads the files of a template folder, refusing symlinks and special files.
fn collect(
    name: &str,
    dir: &Path,
    rel: &str,
    tree: &mut Tree,
    max_bytes: u64,
) -> Result<(), LoadError> {
    if rel.split('/').count() > MAX_DEPTH {
        return Err(load_error(
            name,
            rel,
            format!("a template has at most {MAX_DEPTH} levels of folders"),
        ));
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| load_error(name, rel_or_root(rel), format!("cannot read: {e}")))?
        .collect::<Result<_, _>>()
        .map_err(|e| load_error(name, rel_or_root(rel), format!("cannot read: {e}")))?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let fname = e.file_name().to_string_lossy().to_string();
        let path = if rel.is_empty() {
            fname.clone()
        } else {
            format!("{rel}/{fname}")
        };
        let meta = std::fs::symlink_metadata(e.path())
            .map_err(|err| load_error(name, &path, format!("cannot read: {err}")))?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            return Err(load_error(
                name,
                &path,
                "a symlink in a template is refused (never followed)",
            ));
        }
        if ft.is_dir() {
            if tree.dirs.len() >= MAX_FILES {
                return Err(load_error(
                    name,
                    &path,
                    format!("a template has at most {MAX_FILES} folders"),
                ));
            }
            tree.dirs.insert(path.clone());
            collect(name, &e.path(), &path, tree, max_bytes)?;
        } else if ft.is_file() {
            if tree.files.len() >= MAX_FILES {
                return Err(load_error(
                    name,
                    &path,
                    format!("a template has at most {MAX_FILES} files"),
                ));
            }
            tree.bytes = tree.bytes.saturating_add(meta.len());
            if tree.bytes > max_bytes {
                return Err(load_error(
                    name,
                    &path,
                    format!("a template has at most {max_bytes} bytes"),
                ));
            }
            let data = std::fs::read(e.path())
                .map_err(|err| load_error(name, &path, format!("cannot read: {err}")))?;
            tree.files.insert(path, (data, executable(&meta)));
        } else {
            return Err(load_error(
                name,
                &path,
                "only regular files and folders are allowed",
            ));
        }
    }
    Ok(())
}

fn rel_or_root(rel: &str) -> &str {
    if rel.is_empty() { "/" } else { rel }
}

#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// The database step: the manifest passes the database rules, the migrations' headers parse,
/// the sample data file exists.
fn check_database(name: &str, i: usize, d: &Value, tree: &Tree) -> Result<(), LoadError> {
    let at = |f: &str| format!("/setup/{i}/database/{f}");
    let mpath = d
        .get("manifest")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (raw, _) = tree
        .files
        .get(mpath)
        .ok_or_else(|| load_error(name, at("manifest"), "no such file in the template"))?;
    let text =
        std::str::from_utf8(raw).map_err(|_| load_error(name, at("manifest"), "not UTF-8 text"))?;
    let m = yaml::parse(text).map_err(|e| load_error(name, at("manifest"), e.to_string()))?;
    crate::databases::manifest::check(&m, None, false)
        .map_err(|e| load_error(name, at("manifest"), e.to_api().message))?;
    if let Some(mig) = d.get("migrations").and_then(Value::as_str) {
        let dir = manifest::normal_path(mig);
        if !tree.dirs.contains(&dir) {
            return Err(load_error(
                name,
                at("migrations"),
                "no such folder in the template",
            ));
        }
        let prefix = format!("{dir}/");
        for (k, (v, _)) in tree.files.range(prefix.clone()..) {
            let Some(file) = k.strip_prefix(&prefix) else {
                break;
            };
            if !file.ends_with(".sql") || file.contains('/') {
                continue;
            }
            let sql = std::str::from_utf8(v)
                .map_err(|_| load_error(name, k.as_str(), "not UTF-8 text"))?;
            manifest::parse_classes_header(sql).map_err(|e| load_error(name, k.as_str(), e))?;
        }
    }
    if let Some(s) = d.get("sample_data").and_then(Value::as_str)
        && !tree.files.contains_key(s)
    {
        return Err(load_error(
            name,
            at("sample_data"),
            "no such file in the template",
        ));
    }
    Ok(())
}

/// Unpacks an app archive within the plan's limits and checks its `app.yaml` (the schema and
/// the provider rules; the fail-closed rules of a deploy are the deploy's).
pub fn check_app_archive(bytes: &[u8], plan: &AppPlanLimits) -> Result<(), String> {
    let unpacked = archive::unpack(bytes, &Limits::of(plan)).map_err(|r| match r {
        archive::Refusal::Hostile { entry, reason } => {
            format!("the archive entry {entry:?} is refused: {reason}")
        }
        archive::Refusal::Corrupt { reason } => format!("the archive is not readable: {reason}"),
        archive::Refusal::Limit { what, limit } => {
            format!("the archive is over the limit of {what} ({limit})")
        }
    })?;
    let entry = unpacked
        .get("app.yaml")
        .ok_or_else(|| "the archive has no app.yaml at its root".to_string())?;
    let text =
        std::str::from_utf8(&entry.data).map_err(|_| "app.yaml is not UTF-8 text".to_string())?;
    let m = yaml::parse(text).map_err(|e| format!("app.yaml is not valid YAML: {e}"))?;
    let result = crate::apps::manifest::validate(&m, plan);
    if !result.valid {
        let first = result
            .problems
            .first()
            .map(|p| format!("{}: {}", p.pointer, p.message))
            .unwrap_or_default();
        return Err(format!(
            "app.yaml does not pass the manifest rules: {first}"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------- packing

/// Packs files into a deterministic `.tar.gz`: in the order given (sorted by the caller), ustar
/// headers with mode 0755 for executables and 0644 otherwise, owner and group 0 without names,
/// time 0; gzip with time 0 and no file name. The same files give the same bytes.
pub fn pack_tar_gz(files: &[(String, bool, &[u8])]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut tar = Vec::new();
    for (path, exec, data) in files {
        let mut h = [0u8; 512];
        let (prefix, name) = split_ustar(path)
            .ok_or_else(|| format!("the path {path:?} is too long for an archive"))?;
        h[..name.len()].copy_from_slice(name.as_bytes());
        put_octal(&mut h[100..108], if *exec { 0o755 } else { 0o644 });
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
        tar.extend_from_slice(&h);
        tar.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        tar.extend(std::iter::repeat_n(0u8, pad));
    }
    tar.extend(std::iter::repeat_n(0u8, 1024));
    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::new(6));
    gz.write_all(&tar).map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())
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
