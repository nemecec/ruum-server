//! `template.yaml` (`contracts/manifests/template.schema.json`): parse it, check it against the
//! schema, and the rules that the schema cannot say.
//!
//! Rules on top of the schema:
//!
//! - Every path of `setup` (`folder.path`, `folder.from`, `database.manifest`,
//!   `database.migrations`, `database.sample_data`, `app.archive`) and every placeholder `on` is
//!   relative: not empty, no leading `/`, no `\`, no NUL or control character, no `.` or `..`
//!   segment. So a path can never name a file outside the template or the new project.
//! - `issue.schema` follows [`check_issue`]: an object schema that compiles as JSON Schema draft
//!   2020-12 with no remote `$ref`, at most [`MAX_ISSUE_SCHEMA_BYTES`] and
//!   [`MAX_ISSUE_PROPERTIES`] properties; each `issue.layout` item names a field of the schema.
//!
//! The file is read with the hand-written YAML reader of `app.yaml`
//! ([`crate::apps::deployments::yaml`]): no anchors, aliases, tags or duplicate keys. JSON is
//! YAML too, so a generated `template.yaml` is pretty JSON after one comment line.
//!
//! The classes header of a migration (`-- ruum:classes`) has no parser elsewhere yet;
//! [`parse_classes_header`] reads the provisional form of the gallery (backlog L1-D7).

use crate::apps::deployments::yaml;
use crate::error::{ApiError, Error, ErrorCode};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The normative schema of `template.yaml`.
pub const SCHEMA_JSON: &str = include_str!("../../../../contracts/manifests/template.schema.json");

/// The largest `template.yaml`, in bytes (the limit of the YAML reader).
pub const MAX_MANIFEST_BYTES: usize = yaml::MAX_BYTES;
/// The largest `issue.schema`, as compact JSON.
pub const MAX_ISSUE_SCHEMA_BYTES: usize = 64 * 1024;
/// The most top-level properties of `issue.schema`.
pub const MAX_ISSUE_PROPERTIES: usize = 200;
/// The only `$schema` an issue schema may name (it may also leave it out).
pub const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// One problem of a manifest: a JSON pointer and a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub pointer: String,
    pub message: String,
}

impl Problem {
    pub fn new(pointer: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            pointer: pointer.into(),
            message: message.into(),
        }
    }

    /// The API error: `422 validation_failed` with the pointer in `details.pointer` and in the
    /// hint.
    pub fn into_error(self) -> Error {
        Error::Api(
            ApiError::new(ErrorCode::ValidationFailed, self.message)
                .with_hint(format!(
                    "fix the field at {}",
                    pointer_or_root(&self.pointer)
                ))
                .with_details(json!({ "pointer": self.pointer })),
        )
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", pointer_or_root(&self.pointer), self.message)
    }
}

fn pointer_or_root(p: &str) -> &str {
    if p.is_empty() { "/" } else { p }
}

fn validator() -> &'static jsonschema::Validator {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| {
        let schema: Value =
            serde_json::from_str(SCHEMA_JSON).expect("template.schema.json is JSON");
        jsonschema::validator_for(&schema).expect("template.schema.json compiles")
    })
}

/// Parses the bytes of a `template.yaml`. Errors name a line, never a value.
pub fn parse(bytes: &[u8]) -> Result<Value, Problem> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Problem::new(
            "",
            format!("template.yaml is larger than {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Problem::new("", "template.yaml is not UTF-8 text"))?;
    yaml::parse(text).map_err(|e| Problem::new("", format!("template.yaml is not valid YAML: {e}")))
}

/// Checks a parsed manifest against the schema and the rules of the module docs. Returns the
/// first problem (schema problems first, in the validator's order).
pub fn check(manifest: &Value) -> Result<(), Problem> {
    if let Some(e) = validator().iter_errors(manifest).next() {
        return Err(Problem::new(
            e.instance_path().to_string(),
            format!(
                "template.yaml does not follow the template schema: {}",
                e.masked()
            ),
        ));
    }
    for (pointer, path) in named_paths(manifest) {
        check_relative(&pointer, &path)?;
    }
    if let Some(issue) = manifest.get("issue") {
        check_issue(
            issue.get("schema").unwrap_or(&Value::Null),
            issue.get("layout"),
            "/issue",
        )?;
    }
    Ok(())
}

/// Every path that the manifest names, with its pointer: the paths of `setup` and the `on` of
/// each placeholder.
pub fn named_paths(manifest: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(steps) = manifest.get("setup").and_then(Value::as_array) {
        for (i, step) in steps.iter().enumerate() {
            let Some((kind, body)) = step.as_object().and_then(|o| o.iter().next()) else {
                continue;
            };
            let fields: &[&str] = match kind.as_str() {
                "folder" => &["path", "from"],
                "database" => &["manifest", "migrations", "sample_data"],
                "app" => &["archive"],
                _ => &[],
            };
            for f in fields {
                if let Some(p) = body.get(*f).and_then(Value::as_str) {
                    out.push((format!("/setup/{i}/{kind}/{f}"), p.to_string()));
                }
            }
        }
    }
    if let Some(ph) = manifest.get("placeholders").and_then(Value::as_object) {
        for (name, p) in ph {
            if let Some(on) = p.get("on").and_then(Value::as_str) {
                out.push((format!("/placeholders/{}/on", token(name)), on.to_string()));
            }
        }
    }
    out
}

/// A JSON pointer token (RFC 6901).
fn token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Checks that `path` is relative and stays inside its folder (see the module docs). A trailing
/// `/` is allowed (a folder).
pub fn check_relative(pointer: &str, path: &str) -> Result<(), Problem> {
    let bad = |why: &str| Err(Problem::new(pointer, format!("the path {why}")));
    if path.is_empty() {
        return bad("is empty");
    }
    if path.starts_with('/') {
        return bad("must be relative, not start with /");
    }
    if path.contains('\\') {
        return bad("must use / as the separator, never \\");
    }
    if path.chars().any(char::is_control) {
        return bad("has a control character");
    }
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return bad("names no file or folder");
    }
    for seg in trimmed.split('/') {
        if seg.is_empty() {
            return bad("has an empty segment (//)");
        }
        if seg == "." || seg == ".." {
            return bad("has a . or .. segment");
        }
    }
    Ok(())
}

/// The relative path without a trailing `/` (the key of a file or folder in a template).
pub fn normal_path(path: &str) -> String {
    path.trim_end_matches('/').to_string()
}

/// Checks the fields of an issue template (`issue.schema`, `issue.layout`). `base` is the
/// pointer of the `issue` object in the request or manifest (`/issue`).
pub fn check_issue(schema: &Value, layout: Option<&Value>, base: &str) -> Result<(), Problem> {
    let sp = format!("{base}/schema");
    let Some(obj) = schema.as_object() else {
        return Err(Problem::new(&sp, "the issue schema must be a JSON object"));
    };
    let size = serde_json::to_vec(schema)
        .map(|v| v.len())
        .unwrap_or(usize::MAX);
    if size > MAX_ISSUE_SCHEMA_BYTES {
        return Err(Problem::new(
            &sp,
            format!("the issue schema is larger than {MAX_ISSUE_SCHEMA_BYTES} bytes"),
        ));
    }
    if obj.get("type").and_then(Value::as_str) != Some("object") {
        return Err(Problem::new(
            format!("{sp}/type"),
            "the issue schema must be an object schema (\"type\": \"object\")",
        ));
    }
    if let Some(s) = obj.get("$schema")
        && s.as_str() != Some(DRAFT_2020_12)
    {
        return Err(Problem::new(
            format!("{sp}/$schema"),
            format!("the issue schema must be JSON Schema draft 2020-12 ({DRAFT_2020_12})"),
        ));
    }
    let properties = match obj.get("properties") {
        None => serde_json::Map::new(),
        Some(Value::Object(p)) => p.clone(),
        Some(_) => {
            return Err(Problem::new(
                format!("{sp}/properties"),
                "properties must be an object",
            ));
        }
    };
    if properties.len() > MAX_ISSUE_PROPERTIES {
        return Err(Problem::new(
            format!("{sp}/properties"),
            format!("the issue schema has more than {MAX_ISSUE_PROPERTIES} properties"),
        ));
    }
    if let Some(at) = remote_reference(schema, &sp) {
        return Err(Problem::new(
            at,
            "the issue schema refers to another document; only local references (#...) are \
             allowed",
        ));
    }
    if let Err(e) = jsonschema::draft202012::meta::validate(schema) {
        return Err(Problem::new(
            &sp,
            format!(
                "the issue schema is not valid JSON Schema draft 2020-12: {}",
                e.masked()
            ),
        ));
    }
    if let Err(e) = jsonschema::draft202012::options()
        .with_retriever(NoRemote)
        .build(schema)
    {
        return Err(Problem::new(
            &sp,
            format!("the issue schema does not compile: {}", e.masked()),
        ));
    }
    if let Some(layout) = layout {
        let Some(items) = layout.as_array() else {
            return Err(Problem::new(
                format!("{base}/layout"),
                "layout must be an array",
            ));
        };
        for (i, item) in items.iter().enumerate() {
            let field = item.get("field").and_then(Value::as_str);
            match field {
                Some(f) if properties.contains_key(f) => {}
                _ => {
                    return Err(Problem::new(
                        format!("{base}/layout/{i}/field"),
                        "the layout names a field that is not a property of the issue schema",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// The pointer of the first `$ref`, `$dynamicRef` or `$recursiveRef` that does not start with
/// `#`, and of any `$id` (a new base URI would make a `#` reference point elsewhere).
fn remote_reference(v: &Value, at: &str) -> Option<String> {
    match v {
        Value::Object(o) => {
            for (k, child) in o {
                let here = format!("{at}/{}", token(k));
                match k.as_str() {
                    "$ref" | "$dynamicRef" | "$recursiveRef" => {
                        if !child.as_str().is_some_and(|s| s.starts_with('#')) {
                            return Some(here);
                        }
                    }
                    "$id" => return Some(here),
                    _ => {}
                }
                if let Some(p) = remote_reference(child, &here) {
                    return Some(p);
                }
            }
            None
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .find_map(|(i, c)| remote_reference(c, &format!("{at}/{i}"))),
        _ => None,
    }
}

/// A retriever that fetches nothing: an issue schema never reads another document.
struct NoRemote;

impl jsonschema::Retrieve for NoRemote {
    fn retrieve(
        &self,
        uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err(format!("remote references are not allowed: {}", uri.as_str()).into())
    }
}

/// The data classes of a migration's `-- ruum:classes` header (provisional form, backlog
/// L1-D7): comment lines at the top of the file that start with `-- ruum:classes`, each with
/// entries `<schema>.<table>.<column>=<class>` separated by spaces or commas; the class is
/// `normal`, `personal` or `secret`. Blank lines and other comments may come before the header;
/// the header ends at the first SQL line. An entry that does not follow the form is an error
/// that names the line.
pub fn parse_classes_header(sql: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (no, line) in sql.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let Some(comment) = t.strip_prefix("--") else {
            break;
        };
        let Some(rest) = comment.trim_start().strip_prefix("ruum:classes") else {
            continue;
        };
        for entry in rest.split([' ', ',', '\t']).filter(|e| !e.is_empty()) {
            let (key, class) = entry
                .split_once('=')
                .ok_or_else(|| format!("line {}: an entry is not <column>=<class>", no + 1))?;
            if !matches!(class, "normal" | "personal" | "secret") {
                return Err(format!(
                    "line {}: the class is not normal, personal or secret",
                    no + 1
                ));
            }
            let parts: Vec<&str> = key.split('.').collect();
            let ident = |p: &str| {
                !p.is_empty()
                    && p.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && !p.starts_with(|c: char| c.is_ascii_digit())
            };
            if parts.len() != 3 || !parts.iter().all(|p| ident(p)) {
                return Err(format!(
                    "line {}: a column is not <schema>.<table>.<column> in lower case",
                    no + 1
                ));
            }
            if out.insert(key.to_string(), class.to_string()).is_some() {
                return Err(format!("line {}: a column is named twice", no + 1));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        for ok in [
            "a",
            "a/b",
            "db/migrations/",
            "app.tar.gz",
            ".hidden",
            "a..b",
        ] {
            assert!(check_relative("/p", ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a/./b",
            "./a",
            "a//b",
            "a\\b",
            "a\u{0}b",
        ] {
            assert!(check_relative("/p", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_schema_examples_pass_the_check() {
        let schema: Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        for ex in schema["examples"].as_array().unwrap() {
            check(ex).unwrap();
        }
    }

    #[test]
    fn unknown_fields_and_dotdot_are_refused() {
        let mut m = json!({ "kind": "project", "name": "x", "version": "1" });
        check(&m).unwrap();
        m["surprise"] = json!(true);
        assert!(check(&m).is_err());
        let m = json!({
            "kind": "project", "name": "x", "version": "1",
            "setup": [{ "folder": { "path": "Docs", "from": "../../etc" } }]
        });
        let p = check(&m).unwrap_err();
        assert_eq!(p.pointer, "/setup/0/folder/from");
    }

    #[test]
    fn issue_schema_rules() {
        let ok = json!({ "type": "object", "properties": { "a": { "type": "string" } } });
        check_issue(&ok, Some(&json!([{ "field": "a" }])), "/issue").unwrap();
        // Positive control with a local reference.
        let local = json!({
            "type": "object",
            "$defs": { "s": { "type": "string" } },
            "properties": { "a": { "$ref": "#/$defs/s" } }
        });
        check_issue(&local, None, "/issue").unwrap();
        let remote = json!({
            "type": "object",
            "properties": { "a": { "$ref": "https://example.com/s.json" } }
        });
        let p = check_issue(&remote, None, "/issue").unwrap_err();
        assert_eq!(p.pointer, "/issue/schema/properties/a/$ref");
        let not_object = json!({ "type": "string" });
        assert!(
            check_issue(&not_object, None, "/issue")
                .unwrap_err()
                .pointer
                .starts_with("/issue/schema")
        );
        let big = json!({
            "type": "object",
            "description": "x".repeat(MAX_ISSUE_SCHEMA_BYTES),
        });
        assert_eq!(
            check_issue(&big, None, "/issue").unwrap_err().pointer,
            "/issue/schema"
        );
        let p = check_issue(&ok, Some(&json!([{ "field": "nope" }])), "/issue").unwrap_err();
        assert_eq!(p.pointer, "/issue/layout/0/field");
        let mut many = serde_json::Map::new();
        for i in 0..=MAX_ISSUE_PROPERTIES {
            many.insert(format!("f{i}"), json!({ "type": "string" }));
        }
        let many = json!({ "type": "object", "properties": many });
        assert!(check_issue(&many, None, "/issue").is_err());
    }

    #[test]
    fn classes_header() {
        let sql = "-- a migration\n-- ruum:classes public.notes.title=normal, \
                   public.notes.email=personal\n-- ruum:classes public.notes.token=secret\n\
                   create table notes (id int);\n-- ruum:classes public.x.y=normal\n";
        let c = parse_classes_header(sql).unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(c["public.notes.email"], "personal");
        assert!(parse_classes_header("-- ruum:classes a.b=normal\n").is_err());
        assert!(parse_classes_header("-- ruum:classes a.b.c=public\n").is_err());
    }
}
