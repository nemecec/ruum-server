//! The manifest of a database node (design 8.1; `contracts/manifests/database.schema.json`).
//!
//! The schema is compiled into the binary. A manifest is valid when it passes the schema and the
//! rules of the database provider:
//!
//! - `template` is read-only: a new database has none, and a change keeps the current one.
//! - The engine and its major version never change in step 2 (a major version change is a
//!   planned operation, not a manifest edit). `plan` has one value only (`shared-small`), so the
//!   schema holds it.
//! - The extensions come from the allow-list of the schema (its `enum`), and SQLite has none.
//!   They are fixed when the database is made: the engines of step 2 have no operation that
//!   adds or drops an extension of a running database, so a change fails (`extensions_fixed`).
//! - Only a person changes `data_classes` (decision 0004-5, design 8.5): from an agent, a
//!   manifest whose `data_classes` differ from the current ones (or are not empty for a new
//!   database) fails with hint `suggestColumnClasses`.
//! - `migrations` is a path that no card of step 2 reads (decision 0004-35: step 4 adds a
//!   property for migrations in a repository). It is kept as it is.
//!
//! [`normalize`] fills the defaults of the schema (`extensions`, `backups`, `data_classes`), so
//! the stored manifest always has them.

use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::providers::{Problem, ValidationResult};
use ruum_dbengine::EngineKind;
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

/// The normative schema of the manifest.
pub const SCHEMA_JSON: &str = include_str!("../../../../contracts/manifests/database.schema.json");

/// The hint of an agent's change of `data_classes` (`updateDatabaseManifest`).
pub const HINT_SUGGEST_CLASSES: &str = "suggestColumnClasses";

fn validator() -> &'static jsonschema::Validator {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| {
        let schema: Value =
            serde_json::from_str(SCHEMA_JSON).expect("database.schema.json is JSON");
        jsonschema::validator_for(&schema).expect("database.schema.json compiles")
    })
}

/// The engine part of a valid manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineSpec {
    pub engine: EngineKind,
    /// The major version (`17`, `3`).
    pub version: String,
    pub extensions: Vec<String>,
}

impl EngineSpec {
    /// The engine part of a manifest that passed [`check`].
    pub fn of(manifest: &Value) -> Result<EngineSpec> {
        let engine = match manifest.get("engine").and_then(Value::as_str) {
            Some("postgres") => EngineKind::Postgres,
            Some("sqlite") => EngineKind::Sqlite,
            _ => return Err(problem_error("/engine", "the engine is postgres or sqlite")),
        };
        let version = manifest
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| problem_error("/version", "the version is missing"))?
            .to_string();
        let extensions = manifest
            .get("extensions")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(EngineSpec {
            engine,
            version,
            extensions,
        })
    }
}

/// The name of an engine in a manifest and in `databases.engine`.
pub fn engine_name(kind: EngineKind) -> &'static str {
    match kind {
        EngineKind::Postgres => "postgres",
        EngineKind::Sqlite => "sqlite",
    }
}

/// The manifest with the defaults of the schema filled in: `extensions: []`,
/// `backups: {pitr_days: 7}`, `data_classes: {}`.
pub fn normalize(manifest: &Value) -> Value {
    let mut m = manifest.clone();
    if let Value::Object(o) = &mut m {
        o.entry("extensions").or_insert_with(|| json!([]));
        o.entry("backups")
            .or_insert_with(|| json!({ "pitr_days": 7 }));
        o.entry("data_classes").or_insert_with(|| json!({}));
    }
    m
}

fn data_classes(m: &Value) -> Map<String, Value> {
    m.get("data_classes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn problem(pointer: &str, code: &str, message: impl Into<String>, hint: Option<&str>) -> Problem {
    Problem {
        pointer: pointer.to_string(),
        code: code.to_string(),
        message: message.into(),
        hint: hint.map(str::to_string),
    }
}

/// Validates a manifest against the schema and the provider rules. `current` is the manifest of
/// the node today (`None` for a new database); `by_agent` is true when an agent proposes it.
/// `normalized` is set when the manifest is valid.
pub fn validate(manifest: &Value, current: Option<&Value>, by_agent: bool) -> ValidationResult {
    let mut problems: Vec<Problem> = validator()
        .iter_errors(manifest)
        .map(|e| {
            let pointer = e.instance_path().to_string();
            // The masked text names the rule, not the value.
            problem(&pointer, "schema", e.masked().to_string(), None)
        })
        .collect();
    if !problems.is_empty() {
        return ValidationResult {
            valid: false,
            normalized: None,
            problems,
        };
    }
    let normalized = normalize(manifest);
    let current = current.map(normalize);
    match &current {
        None => {
            if manifest.get("template").is_some() {
                problems.push(problem(
                    "/template",
                    "read_only",
                    "template is set by the platform, never by a client",
                    Some("/template: leave it out"),
                ));
            }
        }
        Some(cur) => {
            if normalized.get("template").is_some()
                && normalized.get("template") != cur.get("template")
            {
                problems.push(problem(
                    "/template",
                    "read_only",
                    "template is set by the platform and cannot change",
                    Some("/template: send the current value or leave it out"),
                ));
            }
            for (key, code, what) in [
                ("engine", "engine_fixed", "the engine"),
                (
                    "version",
                    "major_version_fixed",
                    "the major version of the engine",
                ),
            ] {
                if normalized.get(key) != cur.get(key) {
                    problems.push(problem(
                        &format!("/{key}"),
                        code,
                        format!("{what} cannot change in step 2"),
                        Some(&format!(
                            "/{key}: keep {}",
                            cur.get(key).unwrap_or(&Value::Null)
                        )),
                    ));
                }
            }
            let sorted = |m: &Value| {
                let mut v: Vec<String> = m
                    .get("extensions")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                v.sort();
                v
            };
            if sorted(&normalized) != sorted(cur) {
                problems.push(problem(
                    "/extensions",
                    "extensions_fixed",
                    "the extensions are fixed when the database is made: the engine cannot add \
                     or drop one of a running database yet",
                    Some("/extensions: keep the current list"),
                ));
            }
        }
    }
    if by_agent {
        let before = current.as_ref().map(data_classes).unwrap_or_default();
        if data_classes(&normalized) != before {
            problems.push(problem(
                "/data_classes",
                "data_classes_user_only",
                "only a person changes data_classes",
                Some(HINT_SUGGEST_CLASSES),
            ));
        }
    }
    let mut normalized = normalized;
    if let (Some(cur), Value::Object(o)) = (&current, &mut normalized)
        && let Some(t) = cur.get("template")
    {
        // A client that leaves `template` out keeps the platform's value.
        o.entry("template").or_insert_with(|| t.clone());
    }
    ValidationResult {
        valid: problems.is_empty(),
        normalized: problems.is_empty().then_some(normalized),
        problems,
    }
}

fn problem_error(pointer: &str, message: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(pointer))
}

/// [`validate`] as a result: the normalized manifest, or `validation_failed` with the first
/// problem (its JSON pointer in `hint`, or `suggestColumnClasses` for an agent's change of
/// `data_classes`).
pub fn check(manifest: &Value, current: Option<&Value>, by_agent: bool) -> Result<Value> {
    let r = validate(manifest, current, by_agent);
    if let Some(normalized) = r.normalized {
        return Ok(normalized);
    }
    let first = r
        .problems
        .into_iter()
        .next()
        .unwrap_or_else(|| problem("", "invalid", "the manifest is not valid", None));
    let hint = match &first.hint {
        Some(h) if h == HINT_SUGGEST_CLASSES => h.clone(),
        _ if first.code == "schema" => {
            let p = if first.pointer.is_empty() {
                "/"
            } else {
                &first.pointer
            };
            format!("{p}: see contracts/manifests/database.schema.json")
        }
        Some(h) => h.clone(),
        None => first.pointer.clone(),
    };
    Err(Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            format!("the manifest is not valid: {}", first.message),
        )
        .with_hint(hint),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pg() -> Value {
        json!({ "engine": "postgres", "version": "17", "plan": "shared-small" })
    }

    fn code(r: Result<Value>) -> (String, String) {
        match r {
            Ok(_) => ("ok".into(), String::new()),
            Err(Error::Api(e)) => (e.code.as_str().to_string(), e.hint.unwrap_or_default()),
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn valid_manifests_are_normalized() {
        let m = check(&pg(), None, false).unwrap();
        assert_eq!(
            m,
            json!({
                "engine": "postgres", "version": "17", "plan": "shared-small",
                "extensions": [], "backups": { "pitr_days": 7 }, "data_classes": {}
            })
        );
        let lite = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
        assert!(check(&lite, None, true).is_ok());
        let full = json!({
            "engine": "postgres", "version": "17", "plan": "shared-small",
            "extensions": ["postgis", "pg_trgm"], "backups": { "pitr_days": 7 },
            "data_classes": { "public.plants.name": "normal" }, "migrations": "migrations/"
        });
        assert_eq!(
            check(&full, None, false).unwrap()["migrations"],
            "migrations/"
        );
        // The examples of the schema are valid.
        let schema: Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        for e in schema["examples"].as_array().unwrap() {
            assert!(check(e, None, false).is_ok(), "{e}");
        }
    }

    #[test]
    fn the_schema_holds() {
        for bad in [
            json!({ "engine": "mysql", "version": "8", "plan": "shared-small" }),
            json!({ "engine": "postgres", "version": "16", "plan": "shared-small" }),
            json!({ "engine": "sqlite", "version": "17", "plan": "shared-small" }),
            json!({ "engine": "postgres", "version": "17", "plan": "dedicated" }),
            json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "extensions": ["anon"] }),
            json!({ "engine": "sqlite", "version": "3", "plan": "shared-small", "extensions": ["citext"] }),
            json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "backups": { "pitr_days": 30 } }),
            json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "data_classes": { "a.b.c": "public" } }),
            json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "extra": 1 }),
            json!({ "engine": "postgres", "version": "17" }),
            json!("postgres"),
        ] {
            let (c, hint) = code(check(&bad, None, false));
            assert_eq!(c, "validation_failed", "{bad}");
            assert!(
                hint.starts_with('/'),
                "{bad}: the hint names the pointer: {hint}"
            );
        }
        let (_, hint) = code(check(
            &json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "extensions": ["postgis", "anon"] }),
            None,
            false,
        ));
        assert!(hint.starts_with("/extensions/1"), "{hint}");
    }

    #[test]
    fn the_provider_rules_hold() {
        let cur = check(&pg(), None, false).unwrap();
        // The same manifest again is fine, also from an agent.
        assert!(check(&pg(), Some(&cur), true).is_ok());
        let change = |f: &dyn Fn(&mut Map<String, Value>)| {
            let mut m = cur.clone();
            f(m.as_object_mut().unwrap());
            m
        };
        let sqlite = json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" });
        assert_eq!(
            code(check(&sqlite, Some(&cur), false)).1,
            "/engine: keep \"postgres\""
        );
        let ext = change(&|m| {
            m.insert("extensions".into(), json!(["citext"]));
        });
        assert_eq!(
            code(check(&ext, Some(&cur), false)).1,
            "/extensions: keep the current list"
        );
        // The order of the extensions does not matter.
        let two = check(
            &json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "extensions": ["citext", "hstore"] }),
            None,
            false,
        )
        .unwrap();
        let swapped = json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "extensions": ["hstore", "citext"] });
        assert!(check(&swapped, Some(&two), false).is_ok());
        // template: read-only.
        let tpl = change(&|m| {
            m.insert(
                "template".into(),
                json!({ "node_id": uuid::Uuid::now_v7() }),
            );
        });
        assert_eq!(code(check(&tpl, None, false)).0, "validation_failed");
        assert_eq!(code(check(&tpl, Some(&cur), false)).0, "validation_failed");
        // data_classes: a person changes them, an agent does not.
        let classes = change(&|m| {
            m.insert(
                "data_classes".into(),
                json!({ "public.users.email": "normal" }),
            );
        });
        assert!(check(&classes, Some(&cur), false).is_ok());
        assert_eq!(
            code(check(&classes, Some(&cur), true)),
            ("validation_failed".into(), HINT_SUGGEST_CLASSES.into())
        );
        assert_eq!(
            code(check(&classes, None, true)),
            ("validation_failed".into(), HINT_SUGGEST_CLASSES.into())
        );
        // An agent may resend the current classes.
        let with = check(&classes, Some(&cur), false).unwrap();
        assert!(check(&with, Some(&with), true).is_ok());
        // Leaving data_classes out is the empty map: a change for a database with classes.
        assert_eq!(
            code(check(&pg(), Some(&with), true)).1,
            HINT_SUGGEST_CLASSES
        );
    }

    #[test]
    fn engine_spec_reads_the_engine_part() {
        let m = check(
            &json!({ "engine": "postgres", "version": "17", "plan": "shared-small", "extensions": ["citext"] }),
            None,
            false,
        )
        .unwrap();
        let s = EngineSpec::of(&m).unwrap();
        assert_eq!(s.engine, EngineKind::Postgres);
        assert_eq!(s.version, "17");
        assert_eq!(s.extensions, vec!["citext".to_string()]);
    }

    // ------------------------------------------------------------ the contract test (D-1)

    /// Decision 0004-12 (d): the OpenAPI `DatabaseManifest` has the same keywords, enums and
    /// limits as `database.schema.json`. Only `readOnly`, descriptions and the annotations
    /// `title`, `examples` and `default` may differ, and `$ref`s of the OpenAPI file are
    /// resolved before the comparison.
    #[test]
    fn the_openapi_manifest_equals_the_manifest_schema() {
        let openapi = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/openapi.yaml"),
        )
        .unwrap();
        let schemas = yaml::component_schemas(
            &openapi,
            &["DatabaseManifest", "DatabaseEngine", "DataClass", "Uuid"],
        );
        let api = yaml::resolve(&schemas["DatabaseManifest"], &schemas);
        let json: Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        let json = yaml::resolve_local(&json, &json);
        let (a, b) = (shape(&api, None), shape(&json, None));
        assert_eq!(
            a, b,
            "the OpenAPI DatabaseManifest and contracts/manifests/database.schema.json differ"
        );
        // The comparison sees the limits: a changed limit is a difference.
        let mut changed = json.clone();
        changed["properties"]["extensions"]["maxItems"] = json!(17);
        assert_ne!(shape(&changed, None), a);
        let mut changed = json.clone();
        changed["properties"]["engine"]["enum"] = json!(["postgres"]);
        assert_ne!(shape(&changed, None), a);
        // The marks of decision 0005 B-3 are part of the comparison (backlog 2026-11-09 S-15):
        // a field marked untrusted in one file only is a difference.
        for path in [&["migrations"][..], &["data_classes", "propertyNames"][..]] {
            let mut changed = json.clone();
            let mut at = &mut changed["properties"];
            for key in path {
                at = &mut at[*key];
            }
            assert_eq!(at["x-ruum-content-trust"], json!("untrusted"), "{path:?}");
            at.as_object_mut().unwrap().remove("x-ruum-content-trust");
            assert_ne!(shape(&changed, None), a, "{path:?}");
        }
    }

    /// The validation keywords of a schema, without annotations. Two forms that say the same are
    /// made equal: a `type` in a `then` branch that repeats the type of the same property in
    /// `base`, and a `type` next to a `const` of that type, add nothing and are dropped.
    fn shape(s: &Value, base: Option<&Value>) -> Value {
        const ANNOTATIONS: &[&str] = &[
            "description",
            "title",
            "examples",
            "default",
            "readOnly",
            "$schema",
            "$id",
            "$defs",
        ];
        match s {
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, v) in o {
                    if ANNOTATIONS.contains(&k.as_str()) {
                        continue;
                    }
                    let v = match k.as_str() {
                        "properties" => {
                            let mut props = Map::new();
                            for (name, p) in v.as_object().cloned().unwrap_or_default() {
                                let b = base
                                    .and_then(|b| b.get("properties"))
                                    .and_then(|b| b.get(&name));
                                let mut p = shape(&p, None);
                                if let (Some(b), Value::Object(po)) = (b, &mut p)
                                    && po.get("type").is_some()
                                    && po.get("type") == b.get("type")
                                {
                                    po.remove("type");
                                }
                                props.insert(name, p);
                            }
                            Value::Object(props)
                        }
                        "allOf" => Value::Array(
                            v.as_array()
                                .cloned()
                                .unwrap_or_default()
                                .iter()
                                .map(|branch| {
                                    let mut b = Map::new();
                                    for (bk, bv) in branch.as_object().cloned().unwrap_or_default()
                                    {
                                        let bv = if bk == "then" {
                                            shape(&bv, Some(s))
                                        } else {
                                            shape(&bv, None)
                                        };
                                        b.insert(bk, bv);
                                    }
                                    Value::Object(b)
                                })
                                .collect(),
                        ),
                        _ => shape(v, None),
                    };
                    out.insert(k.clone(), v);
                }
                let implied = match (out.get("const"), out.get("type").and_then(Value::as_str)) {
                    (Some(Value::Number(n)), Some("integer")) => n.is_i64() || n.is_u64(),
                    (Some(Value::String(_)), Some("string")) => true,
                    (Some(Value::Bool(_)), Some("boolean")) => true,
                    _ => false,
                };
                if implied {
                    out.remove("type");
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|v| shape(v, None)).collect()),
            other => other.clone(),
        }
    }

    /// A small reader for the part of `openapi.yaml` that the contract test needs: the block
    /// mappings, block sequences, flow sequences and scalars of `components.schemas`. Block
    /// scalars (`|`, `>-`) are read as text. It fails loudly on what it does not know.
    mod yaml {
        use serde_json::{Map, Value};
        use std::collections::BTreeMap;

        struct Line<'a> {
            indent: usize,
            text: &'a str,
        }

        /// The schemas `names` under `components:` / `schemas:`, by name. Only their own lines
        /// are read.
        pub fn component_schemas(doc: &str, names: &[&str]) -> BTreeMap<String, Value> {
            let lines: Vec<&str> = doc.lines().collect();
            let start = lines
                .iter()
                .position(|l| *l == "components:")
                .expect("components:");
            let schemas = (start..lines.len())
                .find(|&i| lines[i] == "  schemas:")
                .expect("  schemas:");
            let mut out = BTreeMap::new();
            for name in names {
                let head = format!("    {name}:");
                let at = (schemas..lines.len())
                    .find(|&i| lines[i] == head)
                    .unwrap_or_else(|| panic!("no schema {name}"));
                let mut body = Vec::new();
                for l in &lines[at + 1..] {
                    if !l.trim().is_empty() && !l.starts_with("      ") {
                        break;
                    }
                    body.push(*l);
                }
                let parsed: Vec<Line> = body
                    .iter()
                    .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
                    .map(|l| Line {
                        indent: l.len() - l.trim_start().len(),
                        text: l.trim_start(),
                    })
                    .collect();
                let mut pos = 0;
                let v = block(&parsed, &mut pos, 6);
                assert_eq!(pos, parsed.len(), "schema {name}: a line was not read");
                out.insert(name.to_string(), v);
            }
            out
        }

        fn block(lines: &[Line], pos: &mut usize, indent: usize) -> Value {
            if *pos < lines.len()
                && lines[*pos].indent == indent
                && lines[*pos].text.starts_with("- ")
            {
                let mut items = Vec::new();
                while *pos < lines.len()
                    && lines[*pos].indent == indent
                    && lines[*pos].text.starts_with("- ")
                {
                    // `- key: value` starts a mapping item at indent + 2.
                    let rest = &lines[*pos].text[2..];
                    if rest.contains(": ") || rest.ends_with(':') {
                        let mut sub: Vec<Line> = vec![Line {
                            indent: indent + 2,
                            text: rest,
                        }];
                        *pos += 1;
                        while *pos < lines.len() && lines[*pos].indent > indent {
                            sub.push(Line {
                                indent: lines[*pos].indent,
                                text: lines[*pos].text,
                            });
                            *pos += 1;
                        }
                        let mut p = 0;
                        items.push(block(&sub, &mut p, indent + 2));
                    } else {
                        items.push(scalar(rest));
                        *pos += 1;
                    }
                }
                return Value::Array(items);
            }
            let mut map = Map::new();
            while *pos < lines.len() && lines[*pos].indent == indent {
                let text = lines[*pos].text;
                let (key, rest) = match text.split_once(':') {
                    Some((k, r)) if r.is_empty() || r.starts_with(' ') => (k, r.trim()),
                    _ => panic!("not a mapping line: {text}"),
                };
                let key = key.trim_matches('\'').trim_matches('"').to_string();
                *pos += 1;
                let value = if rest.is_empty() {
                    if *pos < lines.len() && lines[*pos].indent > indent {
                        let child = lines[*pos].indent;
                        block(lines, pos, child)
                    } else if *pos < lines.len()
                        && lines[*pos].indent == indent
                        && lines[*pos].text.starts_with("- ")
                    {
                        block(lines, pos, indent)
                    } else {
                        Value::Null
                    }
                } else if rest.starts_with('|') || rest.starts_with('>') {
                    let mut text = String::new();
                    while *pos < lines.len() && lines[*pos].indent > indent {
                        text.push_str(lines[*pos].text);
                        text.push('\n');
                        *pos += 1;
                    }
                    Value::String(text)
                } else {
                    scalar(rest)
                };
                map.insert(key, value);
            }
            Value::Object(map)
        }

        fn scalar(s: &str) -> Value {
            let s = s.trim();
            if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                return Value::Array(
                    split_flow(inner)
                        .into_iter()
                        .filter(|p| !p.trim().is_empty())
                        .map(|p| scalar(&p))
                        .collect(),
                );
            }
            if let Some(inner) = s.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')) {
                return Value::String(inner.replace("''", "'"));
            }
            if let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
                return Value::String(inner.to_string());
            }
            match s {
                "true" => return Value::Bool(true),
                "false" => return Value::Bool(false),
                "null" | "~" => return Value::Null,
                _ => {}
            }
            if let Ok(i) = s.parse::<i64>() {
                return Value::from(i);
            }
            assert!(!s.starts_with('{'), "flow mappings are not read: {s}");
            Value::String(s.to_string())
        }

        fn split_flow(s: &str) -> Vec<String> {
            let mut out = Vec::new();
            let (mut cur, mut depth, mut quote) = (String::new(), 0, None::<char>);
            for c in s.chars() {
                match (c, quote) {
                    ('\'' | '"', None) => {
                        quote = Some(c);
                        cur.push(c);
                    }
                    (q, Some(open)) if q == open => {
                        quote = None;
                        cur.push(c);
                    }
                    ('[', None) => {
                        depth += 1;
                        cur.push(c);
                    }
                    (']', None) => {
                        depth -= 1;
                        cur.push(c);
                    }
                    (',', None) if depth == 0 => out.push(std::mem::take(&mut cur)),
                    _ => cur.push(c),
                }
            }
            out.push(cur);
            out
        }

        /// Replaces every `$ref: '#/components/schemas/X'` by the schema `X` (resolved too).
        pub fn resolve(v: &Value, schemas: &BTreeMap<String, Value>) -> Value {
            match v {
                Value::Object(o) => {
                    if let Some(Value::String(r)) = o.get("$ref") {
                        let name = r
                            .strip_prefix("#/components/schemas/")
                            .unwrap_or_else(|| panic!("unknown $ref {r}"));
                        return resolve(&schemas[name], schemas);
                    }
                    Value::Object(
                        o.iter()
                            .map(|(k, v)| (k.clone(), resolve(v, schemas)))
                            .collect(),
                    )
                }
                Value::Array(a) => Value::Array(a.iter().map(|v| resolve(v, schemas)).collect()),
                other => other.clone(),
            }
        }

        /// Replaces every `$ref: '#/$defs/X'` of a JSON Schema by `$defs/X` of `root`.
        pub fn resolve_local(v: &Value, root: &Value) -> Value {
            match v {
                Value::Object(o) => {
                    if let Some(Value::String(r)) = o.get("$ref") {
                        let name = r
                            .strip_prefix("#/$defs/")
                            .unwrap_or_else(|| panic!("unknown $ref {r}"));
                        return resolve_local(&root["$defs"][name], root);
                    }
                    Value::Object(
                        o.iter()
                            .map(|(k, v)| (k.clone(), resolve_local(v, root)))
                            .collect(),
                    )
                }
                Value::Array(a) => Value::Array(a.iter().map(|v| resolve_local(v, root)).collect()),
                other => other.clone(),
            }
        }
    }
}
