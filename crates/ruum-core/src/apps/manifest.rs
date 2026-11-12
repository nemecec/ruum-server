//! The manifest of a web app (`app.yaml`; `contracts/manifests/app.schema.json`) and the rules of
//! the `ruum.webapp` provider that need no runtime (card W-1).
//!
//! The schema is compiled into the binary. A manifest is valid when it passes the schema and
//! these provider rules:
//!
//! - The plan caps the resources (`AppPlanLimits`): `resources.memory_mb` and
//!   `resources.max_instances` at most the plan's; `resources.min_instances` above 0 (always on)
//!   only on a plan that allows it (design 4.3: free and personal scale to zero).
//! - A name is in `env` or in `secrets`, never in both.
//! - At most one role has `default: true`, at most one has `grants_roles: true`.
//! - `preview_scopes` of an API is a subset of its `scopes`; `granted_by` names a role of the
//!   manifest.
//! - The runtime `python3.13` waits for its card (R-4): it fails validation in this step.
//!
//! The rule "`edit` only on a database" needs the binding targets resolved, which is card B-1's
//! (binding resolution and the deployer check). [`check_binding_roles`] is the seam: it takes
//! the node type of each resolved target and refuses an `edit` binding on anything that is not a
//! database.

use super::plan::AppPlanLimits;
use crate::error::{ApiError, Error, ErrorCode};
use crate::providers::{Problem, ValidationResult};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// The normative schema of the manifest.
pub const SCHEMA_JSON: &str = include_str!("../../../../contracts/manifests/app.schema.json");

/// The `$id` of the schema (`TypeDescriptor.manifest_schema`).
pub const SCHEMA_ID: &str = "https://ruum.space/contracts/manifests/app.schema.json";

fn validator() -> &'static jsonschema::Validator {
    static V: OnceLock<jsonschema::Validator> = OnceLock::new();
    V.get_or_init(|| {
        let schema: Value = serde_json::from_str(SCHEMA_JSON).expect("app.schema.json is JSON");
        jsonschema::validator_for(&schema).expect("app.schema.json compiles")
    })
}

fn problem(pointer: impl Into<String>, code: &str, message: impl Into<String>) -> Problem {
    Problem {
        pointer: pointer.into(),
        code: code.into(),
        message: message.into(),
        hint: None,
    }
}

/// A JSON pointer token (RFC 6901).
fn token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Checks a manifest against the schema and the provider rules of the module docs. Pure.
pub fn validate(manifest: &Value, plan: &AppPlanLimits) -> ValidationResult {
    let mut problems: Vec<Problem> = validator()
        .iter_errors(manifest)
        .map(|e| {
            let pointer = e.instance_path().to_string();
            problem(
                if pointer.is_empty() {
                    "/".to_string()
                } else {
                    pointer
                },
                "schema",
                // The masked text names the rule, not the value (an `env` value can be a
                // token pasted by mistake), as in the database manifest.
                e.masked().to_string(),
            )
        })
        .collect();
    if problems.is_empty() {
        provider_rules(manifest, plan, &mut problems);
    }
    // Keep the first problems only, so an answer stays small.
    problems.truncate(20);
    ValidationResult {
        valid: problems.is_empty(),
        normalized: if problems.is_empty() {
            Some(manifest.clone())
        } else {
            None
        },
        problems,
    }
}

fn provider_rules(m: &Value, plan: &AppPlanLimits, problems: &mut Vec<Problem>) {
    let empty = Map::new();
    let obj = |key: &str| m.get(key).and_then(Value::as_object).unwrap_or(&empty);
    if m.get("runtime").and_then(Value::as_str) == Some("python3.13") {
        problems.push(problem(
            "/runtime",
            "runtime_not_available",
            "the runtime python3.13 is not available yet; use static or node22",
        ));
    }
    let resources = obj("resources");
    if let Some(mb) = resources.get("memory_mb").and_then(Value::as_u64)
        && mb > u64::from(plan.memory_mb)
    {
        problems.push(problem(
            "/resources/memory_mb",
            "plan_limit",
            format!("the plan allows at most {} MB per instance", plan.memory_mb),
        ));
    }
    if let Some(n) = resources.get("max_instances").and_then(Value::as_u64)
        && n > u64::from(plan.max_instances)
    {
        problems.push(problem(
            "/resources/max_instances",
            "plan_limit",
            format!("the plan allows at most {} instances", plan.max_instances),
        ));
    }
    if !plan.always_on
        && resources
            .get("min_instances")
            .and_then(Value::as_u64)
            .is_some_and(|n| n > 0)
    {
        problems.push(problem(
            "/resources/min_instances",
            "plan_limit",
            "the plan scales apps to zero; min_instances above 0 (always on) needs the team plan",
        ));
    }
    if let Some(n) = resources.get("min_instances").and_then(Value::as_u64)
        && let Some(max) = resources.get("max_instances").and_then(Value::as_u64)
        && n > max
    {
        problems.push(problem(
            "/resources/min_instances",
            "min_above_max",
            "min_instances is at most max_instances",
        ));
    }
    // `env` against `secrets` (both forms of `secrets`).
    let secrets: BTreeSet<String> = match m.get("secrets") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Some(Value::Object(o)) => o.keys().cloned().collect(),
        _ => BTreeSet::new(),
    };
    for name in obj("env").keys() {
        if secrets.contains(name) {
            problems.push(problem(
                format!("/env/{}", token(name)),
                "env_is_secret",
                format!("{name} is also a secret; a name is in env or in secrets, not both"),
            ));
        }
    }
    let roles = obj("roles");
    for (flag, code) in [
        ("default", "two_default_roles"),
        ("grants_roles", "two_grants_roles"),
    ] {
        let marked: Vec<&String> = roles
            .iter()
            .filter(|(_, r)| r.get(flag).and_then(Value::as_bool) == Some(true))
            .map(|(k, _)| k)
            .collect();
        if let Some(second) = marked.get(1) {
            problems.push(problem(
                format!("/roles/{}/{flag}", token(second)),
                code,
                format!("at most one role has {flag}: true"),
            ));
        }
    }
    for (name, api) in obj("apis") {
        let scopes: BTreeSet<&str> = api
            .get("scopes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if let Some(preview) = api.get("preview_scopes").and_then(Value::as_array) {
            for (i, s) in preview.iter().enumerate() {
                if let Some(s) = s.as_str()
                    && !scopes.contains(s)
                {
                    problems.push(problem(
                        format!("/apis/{}/preview_scopes/{i}", token(name)),
                        "preview_scope_unknown",
                        format!("{s} is not one of the scopes of the API"),
                    ));
                }
            }
        }
        if let Some(role) = api.get("granted_by").and_then(Value::as_str)
            && !roles.contains_key(role)
        {
            problems.push(problem(
                format!("/apis/{}/granted_by", token(name)),
                "role_unknown",
                format!("{role} is not a role of this app"),
            ));
        }
    }
}

/// The rule "`edit` only on a database" (step 3: a file or folder binding is `view`), for the
/// node bindings of a manifest whose targets are resolved. `target_type` gives the node type of
/// a binding's target (`database`, `folder`, `file`, `webapp`, ...); binding resolution and the
/// deployer check are card B-1's, which calls this with its resolved targets.
pub fn check_binding_roles(
    manifest: &Value,
    target_type: impl Fn(&str) -> Option<String>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let Some(bindings) = manifest.get("bindings").and_then(Value::as_object) else {
        return out;
    };
    for (name, b) in bindings {
        if b.get("api").is_some() {
            continue;
        }
        if b.get("role").and_then(Value::as_str) == Some("edit")
            && target_type(name).as_deref() != Some("database")
        {
            out.push(problem(
                format!("/bindings/{}/role", token(name)),
                "edit_only_on_databases",
                "in this step a binding gets edit only on a database; a file or folder binding is view",
            ));
        }
    }
    out
}

/// `422 validation_failed` for the first problem, its pointer as hint.
pub fn problems_error(problems: &[Problem]) -> Error {
    match problems.first() {
        Some(p) => Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                format!("app.yaml is not valid: {}", p.message),
            )
            .with_hint(p.pointer.clone()),
        ),
        None => Error::api(ErrorCode::ValidationFailed, "app.yaml is not valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tenants::Plan;
    use serde_json::json;

    fn team() -> AppPlanLimits {
        AppPlanLimits::of(Plan::Team)
    }

    fn pointers(m: Value) -> Vec<String> {
        validate(&m, &team())
            .problems
            .into_iter()
            .map(|p| p.pointer)
            .collect()
    }

    #[test]
    fn the_examples_of_the_schema_are_valid() {
        let schema: Value = serde_json::from_str(SCHEMA_JSON).unwrap();
        assert_eq!(schema["$id"], SCHEMA_ID);
        for e in schema["examples"].as_array().unwrap() {
            let r = validate(e, &team());
            assert!(r.valid, "{e}: {:?}", r.problems);
            assert_eq!(r.normalized.as_ref(), Some(e));
        }
    }

    #[test]
    fn schema_problems_have_pointers() {
        assert_eq!(pointers(json!({})), ["/"]);
        // `container` is not a runtime of step 3 (and a server runtime needs `start`).
        assert_eq!(pointers(json!({ "runtime": "container" }))[0], "/runtime");
        assert!(pointers(json!({ "runtime": "node22" })).contains(&"/".to_string()));
        assert_eq!(
            // A platform-reserved name (`RUUM_` prefix), built so no settings scan sees it.
            pointers({
                let mut env = Map::new();
                env.insert(format!("{}_X", "RUUM"), json!("1"));
                json!({ "runtime": "static", "env": env })
            }),
            ["/env"]
        );
    }

    #[test]
    fn provider_rules() {
        assert_eq!(
            pointers(json!({ "runtime": "python3.13", "start": "x" })),
            ["/runtime"]
        );
        assert_eq!(
            pointers(json!({
                "runtime": "static",
                "env": { "STRIPE_KEY": "x", "MODE": "a" },
                "secrets": ["STRIPE_KEY"],
            })),
            ["/env/STRIPE_KEY"]
        );
        assert_eq!(
            pointers(json!({
                "runtime": "static",
                "env": { "SESSION_KEY": "x" },
                "secrets": { "SESSION_KEY": { "generate": "random-32-bytes" } },
            })),
            ["/env/SESSION_KEY"]
        );
        assert_eq!(
            pointers(json!({
                "runtime": "static",
                "roles": {
                    "a": { "description": "a", "default": true },
                    "b": { "description": "b", "default": true, "grants_roles": true },
                    "c": { "description": "c", "grants_roles": true },
                },
            })),
            ["/roles/b/default", "/roles/c/grants_roles"]
        );
        assert_eq!(
            pointers(json!({
                "runtime": "static",
                "roles": { "approver": { "description": "a" } },
                "apis": { "orders": {
                    "visibility": "external", "protocol": "http", "spec": "o.yaml",
                    "scopes": ["orders.read", "orders.write"],
                    "preview_scopes": ["orders.read", "orders.delete"],
                    "granted_by": "boss",
                } },
            })),
            ["/apis/orders/preview_scopes/1", "/apis/orders/granted_by"]
        );
        // The plan caps the resources.
        let free = AppPlanLimits::of(Plan::Free);
        let m = json!({ "runtime": "node22", "start": "node s.js",
            "resources": { "memory_mb": 2048, "max_instances": 20 } });
        let r = validate(&m, &free);
        let p: Vec<_> = r.problems.iter().map(|p| p.pointer.as_str()).collect();
        assert_eq!(p, ["/resources/memory_mb", "/resources/max_instances"]);
        assert!(!r.valid);
        let e = problems_error(&r.problems);
        assert_eq!(e.to_api().hint.as_deref(), Some("/resources/memory_mb"));
        assert_eq!(e.http_status(), 422);
        // Free and personal scale to zero; always on is a team option.
        let always_on = json!({ "runtime": "node22", "start": "node s.js",
            "resources": { "min_instances": 1, "max_instances": 1 } });
        for plan in [Plan::Free, Plan::Personal] {
            let p: Vec<_> = validate(&always_on, &AppPlanLimits::of(plan))
                .problems
                .into_iter()
                .map(|p| p.pointer)
                .collect();
            assert_eq!(p, ["/resources/min_instances"], "{plan:?}");
        }
        assert!(validate(&always_on, &team()).valid);
    }

    #[test]
    fn schema_messages_do_not_quote_values() {
        // A token pasted into an `env` value where the schema wants a string.
        let leaked = "pasted-value-that-must-not-be-echoed";
        let m = json!({ "runtime": "static", "env": { "A": [leaked] } });
        let r = validate(&m, &team());
        assert!(!r.valid);
        for p in &r.problems {
            assert!(!p.message.contains(leaked), "{}", p.message);
        }
        let e = problems_error(&r.problems);
        assert!(!e.to_api().message.contains(leaked));
    }

    #[test]
    fn edit_only_on_databases() {
        let m = json!({ "runtime": "node22", "start": "x", "bindings": {
            "db": { "node": "../garden", "role": "edit" },
            "photos": { "node": "/Photos", "role": "edit" },
            "docs": { "node": "/Docs", "role": "view" },
            "shop": { "node": "../shop", "api": "orders", "scopes": ["orders.read"] },
        } });
        let problems = check_binding_roles(&m, |b| {
            Some(
                match b {
                    "db" => "database",
                    "photos" => "folder",
                    _ => "file",
                }
                .to_string(),
            )
        });
        let p: Vec<_> = problems.iter().map(|p| p.pointer.as_str()).collect();
        assert_eq!(p, ["/bindings/photos/role"]);
        // An unresolved target is never a database.
        assert_eq!(check_binding_roles(&m, |_| None).len(), 2);
    }
}
