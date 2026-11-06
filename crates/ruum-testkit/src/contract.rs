//! Reads the list of operations from `contracts/openapi.yaml`.
//!
//! The suites need only the method, the path template, the `operationId`, whether the
//! operation is public (`security: []`) and whether it is planned (`x-ruum-status: planned`,
//! decision 0004-21: specified, not built yet). A small, strict line scanner reads them, so the test kit
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
    /// True when the operation has `x-ruum-status: planned`: specified but not built. Its route
    /// may be missing or answer `501`; the suites report its checks as planned, not failed.
    pub planned: bool,
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
    // The operation being read.
    struct Cur {
        method: String,
        path: String,
        id: Option<String>,
        public: bool,
        planned: bool,
    }
    let mut cur: Option<Cur> = None;

    let finish = |cur: &mut Option<Cur>, ops: &mut Vec<Operation>| -> Result<(), String> {
        if let Some(c) = cur.take() {
            let id =
                c.id.ok_or_else(|| format!("{} {} has no operationId", c.method, c.path))?;
            ops.push(Operation {
                id,
                method: c.method.to_uppercase(),
                path: c.path,
                public: c.public,
                planned: c.planned,
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
                    cur = Some(Cur {
                        method: key.to_string(),
                        path: p,
                        id: None,
                        public: false,
                        planned: false,
                    });
                }
            }
            6 => {
                if let Some(c) = cur.as_mut() {
                    if let Some(v) = body.strip_prefix("operationId:") {
                        c.id = Some(v.trim().to_string());
                    } else if let Some(v) = body.strip_prefix("security:") {
                        c.public = v.trim() == "[]";
                    } else if let Some(v) = body.strip_prefix("x-ruum-status:") {
                        match v.trim() {
                            "planned" => c.planned = true,
                            other => {
                                return Err(format!(
                                    "line {}: unknown x-ruum-status {other:?} (only `planned`)",
                                    n + 1
                                ));
                            }
                        }
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
    let marks = text
        .lines()
        .filter(|l| l.trim_start().starts_with("x-ruum-status:"))
        .count();
    let planned = ops.iter().filter(|o| o.planned).count();
    if marks != planned {
        return Err(format!(
            "found {planned} planned operations but {marks} x-ruum-status lines; a mark is \
             outside an operation or the layout of the file changed"
        ));
    }
    Ok(ops)
}

/// Whether the operation `id` is planned in `contracts/openapi.yaml`.
pub fn is_planned(id: &str) -> bool {
    use std::sync::OnceLock;
    static PLANNED: OnceLock<std::collections::BTreeSet<String>> = OnceLock::new();
    PLANNED
        .get_or_init(|| {
            operations()
                .into_iter()
                .filter(|o| o.planned)
                .map(|o| o.id)
                .collect()
        })
        .contains(id)
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
      x-ruum-status: planned
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
        assert!(ops[2].planned);
        assert!(!ops[1].planned);
        assert!(parse(&text.replace("x-ruum-status: planned", "x-ruum-status: built")).is_err());
    }

    #[test]
    fn matches_templates() {
        let op = Operation {
            id: "x".into(),
            method: "GET".into(),
            path: "/v1/nodes/{node_id}/versions/{version_id}".into(),
            public: false,
            planned: false,
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
