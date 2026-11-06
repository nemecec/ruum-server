//! Checks `contracts/mcp/tools.yaml` against `contracts/openapi.yaml`:
//!
//! - every tool maps to an existing `operationId`, and its `input` points at that operation;
//! - tool names are unique snake_case `verb_noun` names, each with a one-line description;
//! - every operation of the contract is in exactly one list (`tools` or `not_exposed`);
//! - operations marked `x-ruum-user-only` are never tools.
//!
//! Small line scanners read both files, so the crate needs no YAML library. They depend on the
//! layout of the files and fail loudly when it changes.

use std::collections::{BTreeMap, BTreeSet};

const OPENAPI: &str = include_str!("../../../contracts/openapi.yaml");
const TOOLS: &str = include_str!("../../../contracts/mcp/tools.yaml");

const METHODS: &[&str] = &["get", "put", "post", "delete", "patch", "head", "options"];

/// One operation of the contract.
#[derive(Debug)]
struct Operation {
    path: String,
    method: String,
    user_only: bool,
}

/// The operations of `openapi.yaml` by `operationId`.
fn operations() -> BTreeMap<String, Operation> {
    let mut ops = BTreeMap::new();
    let mut in_paths = false;
    let mut path = String::new();
    let mut method = String::new();
    let mut current: Option<String> = None;
    for line in OPENAPI.lines() {
        if line == "paths:" {
            in_paths = true;
            continue;
        }
        if in_paths && !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
            in_paths = false;
        }
        if !in_paths {
            continue;
        }
        if let Some(p) = line.strip_prefix("  /") {
            path = format!(
                "/{}",
                p.strip_suffix(':').expect("a path key ends with ':'")
            );
            current = None;
        } else if let Some(m) = line.strip_prefix("    ").and_then(|l| l.strip_suffix(':')) {
            if METHODS.contains(&m) {
                method = m.to_string();
                current = None;
            }
        } else if let Some(id) = line.strip_prefix("      operationId: ") {
            let id = id.trim().to_string();
            let old = ops.insert(
                id.clone(),
                Operation {
                    path: path.clone(),
                    method: method.clone(),
                    user_only: false,
                },
            );
            assert!(old.is_none(), "operationId {id} twice");
            current = Some(id);
        } else if line == "      x-ruum-user-only: true" {
            let id = current
                .as_ref()
                .expect("x-ruum-user-only after the operationId");
            ops.get_mut(id).unwrap().user_only = true;
        }
    }
    let count = OPENAPI.matches("operationId: ").count();
    assert_eq!(ops.len(), count, "the scanner missed operations");
    ops
}

/// One entry of `tools` or `not_exposed`.
#[derive(Debug, Default)]
struct Entry {
    name: Option<String>,
    operation: String,
    description: Option<String>,
    input: Option<String>,
    reason: Option<String>,
}

/// The `tools` and `not_exposed` lists of `tools.yaml`.
fn tool_lists() -> (Vec<Entry>, Vec<Entry>) {
    let mut tools: Vec<Entry> = Vec::new();
    let mut hidden: Vec<Entry> = Vec::new();
    let mut section = "";
    for line in TOOLS.lines() {
        match line {
            "tools:" => {
                section = "tools";
                continue;
            }
            "not_exposed:" => {
                section = "not_exposed";
                continue;
            }
            _ => {}
        }
        let list = match section {
            "tools" => &mut tools,
            "not_exposed" => &mut hidden,
            _ => continue,
        };
        if let Some(rest) = line.strip_prefix("  - ") {
            list.push(Entry::default());
            field(list.last_mut().unwrap(), rest);
        } else if let Some(rest) = line.strip_prefix("    ") {
            field(
                list.last_mut().expect("a field before the first entry"),
                rest,
            );
        } else {
            assert!(
                line.trim().is_empty(),
                "unexpected line in tools.yaml: {line}"
            );
        }
    }
    (tools, hidden)
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        inner.replace("''", "'")
    } else {
        v.to_string()
    }
}

fn field(e: &mut Entry, kv: &str) {
    let (k, v) = kv.split_once(": ").expect("key: value");
    let v = unquote(v);
    match k {
        "name" => e.name = Some(v),
        "operation" => e.operation = v,
        "description" => e.description = Some(v),
        "input" => e.input = Some(v),
        "reason" => e.reason = Some(v),
        "notes" | "note" => {}
        other => panic!("unknown field {other} in tools.yaml"),
    }
}

/// The JSON pointer of an operation (`#/paths/~1v1~1nodes~1{node_id}/get`).
fn pointer(op: &Operation) -> String {
    let p = op.path.replace('~', "~0").replace('/', "~1");
    format!("#/paths/{p}/{}", op.method)
}

#[test]
fn every_tool_maps_to_an_existing_operation() {
    let ops = operations();
    let (tools, _) = tool_lists();
    assert!(!tools.is_empty());
    for t in &tools {
        let name = t.name.as_deref().expect("a tool has a name");
        let op = ops
            .get(&t.operation)
            .unwrap_or_else(|| panic!("tool {name}: no operationId {}", t.operation));
        assert_eq!(
            t.input.as_deref(),
            Some(pointer(op).as_str()),
            "tool {name}: input must point at operation {}",
            t.operation
        );
        assert!(
            !op.user_only,
            "tool {name}: {} is x-ruum-user-only",
            t.operation
        );
    }
}

#[test]
fn tool_names_are_unique_verb_noun_with_a_description() {
    let (tools, _) = tool_lists();
    let mut names = BTreeSet::new();
    for t in &tools {
        let name = t.name.clone().unwrap();
        let ok = name.contains('_')
            && name
                .split('_')
                .all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase()));
        assert!(ok, "tool name {name} is not snake_case verb_noun");
        assert!(names.insert(name.clone()), "tool name {name} twice");
        let d = t.description.as_deref().unwrap_or_default();
        assert!(
            !d.is_empty() && !d.contains('\n'),
            "tool {name}: one-line description"
        );
    }
}

#[test]
fn every_operation_is_listed_once() {
    let ops = operations();
    let (tools, hidden) = tool_lists();
    let mut listed = BTreeSet::new();
    for e in tools.iter().chain(hidden.iter()) {
        assert!(
            ops.contains_key(&e.operation),
            "tools.yaml names unknown operationId {}",
            e.operation
        );
        assert!(
            listed.insert(e.operation.clone()),
            "{} is listed twice",
            e.operation
        );
    }
    let all: BTreeSet<String> = ops.keys().cloned().collect();
    let missing: Vec<_> = all.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "operations without a decision: {missing:?}"
    );
}

#[test]
fn user_only_operations_are_not_exposed() {
    let ops = operations();
    let (_, hidden) = tool_lists();
    for (id, op) in &ops {
        if op.user_only {
            let e = hidden
                .iter()
                .find(|e| &e.operation == id)
                .unwrap_or_else(|| panic!("{id} is user-only but not in not_exposed"));
            assert_eq!(e.reason.as_deref(), Some("user_only"), "{id}");
        }
    }
    for e in &hidden {
        let reason = e.reason.as_deref().unwrap_or_default();
        assert!(
            matches!(reason, "user_only" | "not_for_agents"),
            "{}: reason {reason}",
            e.operation
        );
    }
}

/// `src/tools.json` (what the server serves) is generated from the current contract.
#[test]
fn tools_json_is_up_to_date() {
    use sha2::{Digest, Sha256};
    let catalog = ruum_mcp::Catalog::load();
    let regen = "run `node crates/ruum-mcp/scripts/gen-tools.mjs` from the repository root";
    assert_eq!(
        catalog.openapi_sha256,
        hex::encode(Sha256::digest(OPENAPI.as_bytes())),
        "contracts/openapi.yaml changed: {regen}"
    );
    assert_eq!(
        catalog.tools_sha256,
        hex::encode(Sha256::digest(TOOLS.as_bytes())),
        "contracts/mcp/tools.yaml changed: {regen}"
    );
    let ops = operations();
    let (tools, _) = tool_lists();
    assert_eq!(catalog.tools.len(), tools.len());
    for (spec, entry) in catalog.tools.iter().zip(&tools) {
        assert_eq!(Some(&spec.name), entry.name.as_ref());
        assert_eq!(spec.operation, entry.operation);
        let op = &ops[&spec.operation];
        assert_eq!(spec.path, op.path, "{}", spec.name);
        assert_eq!(spec.method, op.method.to_uppercase(), "{}", spec.name);
        // Every `{param}` of the path is a required input property.
        let required: Vec<&str> = spec
            .input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        for seg in spec.path.split('/') {
            if let Some(p) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                assert!(required.contains(&p), "{}: {p} is not required", spec.name);
            }
        }
    }
}
