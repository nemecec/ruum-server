//! Reads the list of operations from `contracts/openapi.yaml`.
//!
//! The suites need only the method, the path template, the `operationId` and whether the
//! operation is public (`security: []`). A small, strict line scanner reads them, so the test kit
//! needs no YAML library. It depends on the layout of the file: path keys indented by two spaces
//! under `paths:`, methods by four, and operation fields by six. If the layout changes, the
//! scanner fails loudly; it never skips an operation in silence (it compares its count with the
//! number of `operationId:` lines in the file).

use std::path::PathBuf;

/// One operation of the contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    /// The `operationId`, for example `getNode`.
    pub id: String,
    /// Upper case: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`.
    pub method: String,
    /// The path template, for example `/v1/nodes/{node_id}`.
    pub path: String,
    /// True when the operation has `security: []` (no sign-in needed).
    pub public: bool,
}

impl Operation {
    /// Whether a concrete path (without the query) fits the template of this operation.
    /// A `{path}` parameter may span several segments; all other parameters are one segment.
    pub fn matches(&self, concrete: &str) -> bool {
        let concrete = concrete.split('?').next().unwrap_or_default();
        let tpl: Vec<&str> = self.path.split('/').collect();
        let got: Vec<&str> = concrete.split('/').collect();
        match_segments(&tpl, &got)
    }
}

fn match_segments(tpl: &[&str], got: &[&str]) -> bool {
    match (tpl.first(), got.first()) {
        (None, None) => true,
        (Some(t), Some(_)) if *t == "{path}" => {
            // The rest of the URL, one segment or more.
            tpl.len() == 1 && !got.is_empty() && got.iter().all(|s| !s.is_empty())
        }
        (Some(t), Some(g)) if t.starts_with('{') && t.ends_with('}') => {
            !g.is_empty() && match_segments(&tpl[1..], &got[1..])
        }
        (Some(t), Some(g)) => t == g && match_segments(&tpl[1..], &got[1..]),
        _ => false,
    }
}

const METHODS: &[&str] = &["get", "put", "post", "delete", "patch", "head", "options"];

/// The path of `contracts/openapi.yaml` in this repository.
pub fn openapi_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/openapi.yaml")
}

/// All operations of `contracts/openapi.yaml`, in file order.
pub fn operations() -> Vec<Operation> {
    let path = openapi_path();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    parse(&text).unwrap_or_else(|e| panic!("scan {}: {e}", path.display()))
}

/// Scans the text of an OpenAPI file. See the module comment for the layout it needs.
pub fn parse(text: &str) -> Result<Vec<Operation>, String> {
    let mut ops: Vec<Operation> = Vec::new();
    let mut in_paths = false;
    let mut path: Option<String> = None;
    // The operation being read: (method, path, id, public).
    let mut cur: Option<(String, String, Option<String>, bool)> = None;

    let finish = |cur: &mut Option<(String, String, Option<String>, bool)>,
                  ops: &mut Vec<Operation>|
     -> Result<(), String> {
        if let Some((method, path, id, public)) = cur.take() {
            let id = id.ok_or_else(|| format!("{method} {path} has no operationId"))?;
            ops.push(Operation {
                id,
                method: method.to_uppercase(),
                path,
                public,
            });
        }
        Ok(())
    };

    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            finish(&mut cur, &mut ops)?;
            in_paths = line == "paths:";
            path = None;
            continue;
        }
        if !in_paths {
            continue;
        }
        let body = line.trim_start();
        match indent {
            2 => {
                finish(&mut cur, &mut ops)?;
                let key = body
                    .strip_suffix(':')
                    .ok_or_else(|| format!("line {}: expected a path key", n + 1))?;
                let key = key.trim_matches(|c| c == '\'' || c == '"');
                if !key.starts_with('/') {
                    return Err(format!(
                        "line {}: path key {key:?} must start with /",
                        n + 1
                    ));
                }
                path = Some(key.to_string());
            }
            4 => {
                finish(&mut cur, &mut ops)?;
                let key = body.strip_suffix(':').unwrap_or(body);
                if METHODS.contains(&key) {
                    let p = path
                        .clone()
                        .ok_or_else(|| format!("line {}: method outside a path", n + 1))?;
                    cur = Some((key.to_string(), p, None, false));
                }
            }
            6 => {
                if let Some(c) = cur.as_mut() {
                    if let Some(v) = body.strip_prefix("operationId:") {
                        c.2 = Some(v.trim().to_string());
                    } else if let Some(v) = body.strip_prefix("security:") {
                        c.3 = v.trim() == "[]";
                    }
                }
            }
            _ => {}
        }
    }
    finish(&mut cur, &mut ops)?;

    let expected = text
        .lines()
        .filter(|l| l.trim_start().starts_with("operationId:"))
        .count();
    if expected != ops.len() {
        return Err(format!(
            "found {} operations but {expected} operationId lines; the layout of the file changed",
            ops.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for op in &ops {
        if !seen.insert(op.id.clone()) {
            return Err(format!("operationId {} is not unique", op.id));
        }
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_a_small_file() {
        let text = "\
openapi: 3.1.0
paths:
  /healthz:
    get:
      operationId: health
      security: []
  /v1/nodes/{node_id}:
    parameters:
      - $ref: '#/components/parameters/NodeId'
    get:
      operationId: getNode
      description: |
        operationId: inside a text block is not counted
    delete:
      operationId: deleteNode
components:
  schemas: {}
";
        // The text block line has more indent and is not a field; but it is counted by the
        // cross-check, so the cross-check must fail here.
        assert!(parse(text).is_err());
        let text = text.replace(
            "        operationId: inside a text block is not counted\n",
            "",
        );
        let ops = parse(&text).unwrap();
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].id, "health");
        assert!(ops[0].public);
        assert_eq!(ops[1].method, "GET");
        assert_eq!(ops[1].path, "/v1/nodes/{node_id}");
        assert!(!ops[1].public);
        assert_eq!(ops[2].method, "DELETE");
    }

    #[test]
    fn matches_templates() {
        let op = Operation {
            id: "x".into(),
            method: "GET".into(),
            path: "/v1/nodes/{node_id}/versions/{version_id}".into(),
            public: false,
        };
        assert!(op.matches("/v1/nodes/a/versions/b"));
        assert!(op.matches("/v1/nodes/a/versions/b?x=1"));
        assert!(!op.matches("/v1/nodes/a/versions"));
        assert!(!op.matches("/v1/nodes/a/versions/b/c"));
        let by_path = Operation {
            path: "/v1/nodes/by-path/{path}".into(),
            ..op
        };
        assert!(by_path.matches("/v1/nodes/by-path/Projects/plan.txt"));
        assert!(!by_path.matches("/v1/nodes/by-path/"));
    }

    #[test]
    fn the_contract_scans() {
        let ops = operations();
        assert!(ops.len() >= 70, "only {} operations", ops.len());
        assert!(ops.iter().any(|o| o.id == "getNode" && o.method == "GET"));
    }
}
