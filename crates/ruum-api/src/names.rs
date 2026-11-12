//! Names of principals in answers (contract need C-7, decision 0003-5).
//!
//! Every answer that names a principal carries its name: `PrincipalRef.display_name`, the
//! `on_behalf_of_display_name` next to every `on_behalf_of`, and `display_name` of a user
//! grantee. One middleware ([`fill_names`]) does this for all operations, also for MCP tools,
//! which run through the same router: after the handler, it reads the JSON answer, collects
//! the principal IDs, resolves them with one `TenantStore::principal_names` call per tenant,
//! and writes the names in.
//!
//! Rules:
//!
//! - Names are filled only when the request was authenticated as a user or an agent and the
//!   handler built a tenant context ([`note_tenant`]) or a signed-in principal ([`note_signed_in`]).
//!   Share-link bearers get no names.
//! - The tenant of a principal is the `tenant_id` of the nearest enclosing object (or of its
//!   `node`), else the tenant of the request. So "Shared with me", which spans tenants, names
//!   each sharer in the item's tenant.
//! - The resolver looks only in that tenant (under its row-level security), so a name never
//!   crosses tenants.
//! - The email of a user grantee is filled only when the handler allowed it
//!   ([`allow_grantee_emails`]): callers who may share the node and may list the members.
//! - Names go only where the contract types a field as a principal (backlog 2026-11-09 S-11):
//!   a `PrincipalRef`, an object with `on_behalf_of_display_name` next to its `on_behalf_of`,
//!   and a `GranteeUser`. The positions of each operation's answer come from
//!   `contracts/openapi.yaml` (`principal_paths.json`, made by
//!   `scripts/gen-principal-paths.mjs`); a field marked `x-ruum-content-trust: untrusted` is
//!   never entered. So user-made JSON (the rows of a query, a manifest, the `data` of an audit
//!   entry, the properties of a typed node) never gets a name, even when it is shaped like
//!   `{"kind": "user", "id": ...}` with a `tenant_id` (backlog S-3). An answer of a path that
//!   the contract does not have gets no names.

use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use ruum_core::tenants::PrincipalName;
use ruum_core::{Context, PrincipalKind, TenantId};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default)]
struct Scope {
    signed_in: bool,
    tenant: Option<TenantId>,
    grantee_emails: bool,
}

tokio::task_local! {
    static SCOPE: Arc<Mutex<Scope>>;
}

fn update(f: impl FnOnce(&mut Scope)) {
    let _ = SCOPE.try_with(|s| {
        if let Ok(mut s) = s.lock() {
            f(&mut s);
        }
    });
}

/// The handler of this request has a signed-in user or agent (not a share-link bearer).
pub fn note_signed_in(kind: PrincipalKind) {
    if kind != PrincipalKind::ShareLink {
        update(|s| s.signed_in = true);
    }
}

/// The handler of this request runs in the tenant of `ctx`.
pub fn note_tenant(ctx: &Context) {
    if ctx.actor.actor.kind != PrincipalKind::ShareLink {
        update(|s| {
            s.signed_in = true;
            s.tenant = Some(ctx.tenant);
        });
    }
}

/// The caller may see the emails of user grantees in this answer.
pub fn allow_grantee_emails() {
    update(|s| s.grantee_emails = true);
}

/// The positions of the principals in the answer of one operation (`principal_paths.json`).
/// A position is a list of steps: a property name, or `*` for every item of an array and every
/// value of a map.
#[derive(Debug, Default, Deserialize)]
struct Positions {
    method: String,
    path: String,
    /// `PrincipalRef` objects.
    refs: Vec<Vec<String>>,
    /// Objects with `on_behalf_of` and `on_behalf_of_display_name`.
    obo: Vec<Vec<String>>,
    /// `GranteeUser` objects.
    grantees: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct PrincipalPaths {
    #[cfg_attr(not(test), allow(dead_code))]
    openapi_sha256: String,
    operations: Vec<Positions>,
}

static PRINCIPAL_PATHS: LazyLock<PrincipalPaths> = LazyLock::new(|| {
    serde_json::from_str(include_str!("principal_paths.json"))
        .expect("principal_paths.json is the output of scripts/gen-principal-paths.mjs")
});

/// The positions of the operation that answers `method` and `path` (a path template of the
/// contract; a literal segment wins over a `{parameter}`), or `None`.
fn positions_for(method: &str, path: &str) -> Option<&'static Positions> {
    let segments: Vec<&str> = path.split('/').collect();
    PRINCIPAL_PATHS
        .operations
        .iter()
        .filter(|o| o.method.eq_ignore_ascii_case(method))
        .filter_map(|o| {
            let template: Vec<&str> = o.path.split('/').collect();
            if template.len() != segments.len() {
                return None;
            }
            let mut literal = 0;
            for (t, s) in template.iter().zip(&segments) {
                if t.starts_with('{') && t.ends_with('}') {
                    if s.is_empty() {
                        return None;
                    }
                } else if t == s {
                    literal += 1;
                } else {
                    return None;
                }
            }
            Some((literal, o))
        })
        .max_by_key(|(literal, _)| *literal)
        .map(|(_, o)| o)
}

/// The middleware: runs the request with a fresh scope, then fills the names into a JSON answer.
pub async fn fill_names(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let positions = positions_for(req.method().as_str(), req.uri().path());
    let scope = Arc::new(Mutex::new(Scope::default()));
    let res = SCOPE.scope(scope.clone(), next.run(req)).await;
    let scope = scope.lock().map(|s| *s).unwrap_or_default();
    let Some(positions) = positions else {
        return res;
    };
    if !scope.signed_in || !res.status().is_success() || !is_json(&res) {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "names: reading the answer failed");
            return Response::from_parts(parts, Body::empty());
        }
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let mut wanted: HashMap<TenantId, HashSet<Uuid>> = HashMap::new();
    walk(&value, positions, scope.tenant, &mut |t, id| {
        wanted.entry(t).or_default().insert(id);
    });
    if wanted.is_empty() {
        return Response::from_parts(parts, Body::from(bytes));
    }
    let mut names: HashMap<(TenantId, Uuid), PrincipalName> = HashMap::new();
    for (t, ids) in wanted {
        let ids: Vec<Uuid> = ids.into_iter().collect();
        match state.services.tenants.principal_names(t, &ids).await {
            Ok(found) => names.extend(found.into_iter().map(|(id, n)| ((t, id), n))),
            // Names are optional fields: an answer without them is still correct.
            Err(e) => tracing::warn!(error = %e, tenant = %t, "names: lookup failed"),
        }
    }
    fill(
        &mut value,
        positions,
        scope.tenant,
        &names,
        scope.grantee_emails,
    );
    let Ok(out) = serde_json::to_vec(&value) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    if let Ok(len) = HeaderValue::from_str(&out.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, len);
    }
    Response::from_parts(parts, Body::from(out))
}

fn is_json(res: &Response) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            let v = v.split(';').next().unwrap_or_default().trim();
            v.eq_ignore_ascii_case("application/json")
        })
}

fn uuid_of(v: Option<&Value>) -> Option<Uuid> {
    v.and_then(Value::as_str).and_then(|s| s.parse().ok())
}

/// The tenant of the principals inside `obj`: its `tenant_id`, or its node's, else `outer`.
fn tenant_of(obj: &Map<String, Value>, outer: Option<TenantId>) -> Option<TenantId> {
    uuid_of(obj.get("tenant_id"))
        .or_else(|| {
            obj.get("node")
                .and_then(Value::as_object)
                .and_then(|n| uuid_of(n.get("tenant_id")))
        })
        .map(TenantId)
        .or(outer)
}

/// A `PrincipalRef`: `kind` and `id`, maybe `display_name`, nothing else. Share-link bearers
/// have no name.
fn principal_ref(obj: &Map<String, Value>) -> Option<Uuid> {
    if !obj
        .keys()
        .all(|k| matches!(k.as_str(), "kind" | "id" | "display_name"))
    {
        return None;
    }
    match obj.get("kind").and_then(Value::as_str)? {
        "user" | "agent" | "app" | "group" => uuid_of(obj.get("id")),
        _ => None,
    }
}

/// A user grantee (`GranteeUser`): `type: user` and `id`.
fn user_grantee(obj: &Map<String, Value>) -> Option<Uuid> {
    if !obj
        .keys()
        .all(|k| matches!(k.as_str(), "type" | "id" | "display_name" | "email"))
        || obj.get("type").and_then(Value::as_str) != Some("user")
    {
        return None;
    }
    uuid_of(obj.get("id"))
}

/// Calls `f` with every object at `steps` below `v`, and the tenant of its principals (the
/// `tenant_id` of the nearest enclosing typed object, [`tenant_of`]).
fn visit<'a>(
    v: &'a Value,
    steps: &[String],
    tenant: Option<TenantId>,
    f: &mut dyn FnMut(&'a Map<String, Value>, Option<TenantId>),
) {
    match (steps.split_first(), v) {
        (None, Value::Object(obj)) => f(obj, tenant_of(obj, tenant)),
        (Some((step, rest)), Value::Array(items)) if step == "*" => {
            items.iter().for_each(|i| visit(i, rest, tenant, f));
        }
        // A map (`additionalProperties`): its keys are data, so its own fields set no tenant.
        (Some((step, rest)), Value::Object(obj)) if step == "*" => {
            obj.values().for_each(|c| visit(c, rest, tenant, f));
        }
        (Some((step, rest)), Value::Object(obj)) => {
            if let Some(c) = obj.get(step) {
                visit(c, rest, tenant_of(obj, tenant), f);
            }
        }
        _ => {}
    }
}

/// What [`visit_mut`] calls for each object it reaches.
type Change<'f> = &'f mut dyn FnMut(&mut Map<String, Value>, Option<TenantId>);

/// [`visit`] for a change.
fn visit_mut(v: &mut Value, steps: &[String], tenant: Option<TenantId>, f: Change<'_>) {
    match (steps.split_first(), v) {
        (None, Value::Object(obj)) => {
            let t = tenant_of(obj, tenant);
            f(obj, t)
        }
        (Some((step, rest)), Value::Array(items)) if step == "*" => {
            items.iter_mut().for_each(|i| visit_mut(i, rest, tenant, f));
        }
        (Some((step, rest)), Value::Object(obj)) if step == "*" => {
            obj.values_mut().for_each(|c| visit_mut(c, rest, tenant, f));
        }
        (Some((step, rest)), Value::Object(obj)) => {
            let t = tenant_of(obj, tenant);
            if let Some(c) = obj.get_mut(step) {
                visit_mut(c, rest, t, f);
            }
        }
        _ => {}
    }
}

/// The principals at the positions of an answer.
fn walk(
    v: &Value,
    positions: &Positions,
    tenant: Option<TenantId>,
    found: &mut dyn FnMut(TenantId, Uuid),
) {
    for steps in &positions.refs {
        visit(v, steps, tenant, &mut |obj, t| {
            if let (Some(t), Some(id)) = (t, principal_ref(obj)) {
                found(t, id);
            }
        });
    }
    for steps in &positions.obo {
        visit(v, steps, tenant, &mut |obj, t| {
            if let (Some(t), Some(id)) = (t, uuid_of(obj.get("on_behalf_of"))) {
                found(t, id);
            }
        });
    }
    for steps in &positions.grantees {
        visit(v, steps, tenant, &mut |obj, t| {
            if let (Some(t), Some(id)) = (t, user_grantee(obj)) {
                found(t, id);
            }
        });
    }
}

fn fill(
    v: &mut Value,
    positions: &Positions,
    tenant: Option<TenantId>,
    names: &HashMap<(TenantId, Uuid), PrincipalName>,
    grantee_emails: bool,
) {
    for steps in &positions.refs {
        visit_mut(v, steps, tenant, &mut |obj, t| {
            if let Some(t) = t
                && let Some(n) = principal_ref(obj).and_then(|id| names.get(&(t, id)))
            {
                obj.insert("display_name".into(), n.display_name.clone().into());
            }
        });
    }
    for steps in &positions.obo {
        visit_mut(v, steps, tenant, &mut |obj, t| {
            if let Some(t) = t
                && let Some(n) = uuid_of(obj.get("on_behalf_of")).and_then(|id| names.get(&(t, id)))
            {
                obj.insert(
                    "on_behalf_of_display_name".into(),
                    n.display_name.clone().into(),
                );
            }
        });
    }
    for steps in &positions.grantees {
        visit_mut(v, steps, tenant, &mut |g, t| {
            if let Some(t) = t
                && let Some(n) = user_grantee(g).and_then(|id| names.get(&(t, id)))
            {
                g.insert("display_name".into(), n.display_name.clone().into());
                match (&n.email, grantee_emails) {
                    (Some(e), true) => {
                        g.insert("email".into(), e.clone().into());
                    }
                    _ => {
                        g.remove("email");
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn name(n: &str, email: Option<&str>) -> PrincipalName {
        PrincipalName {
            display_name: n.into(),
            email: email.map(Into::into),
            owner: None,
        }
    }

    fn steps(list: &[&[&str]]) -> Vec<Vec<String>> {
        list.iter()
            .map(|p| p.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn fills_refs_obo_and_grantees_in_their_tenant() {
        let (t1, t2) = (TenantId(Uuid::now_v7()), TenantId(Uuid::now_v7()));
        let [alice, bot, bob, carol, dave] = [(); 5].map(|_| Uuid::now_v7());
        let positions = Positions {
            refs: steps(&[
                &["items", "*", "actor"],
                &["items", "*", "shared_by", "actor"],
            ]),
            obo: steps(&[&["items", "*"], &["items", "*", "shared_by"]]),
            grantees: steps(&[&["items", "*", "grantee"]]),
            ..Positions::default()
        };
        let mut v = json!({
            "items": [
                {"actor": {"kind": "agent", "id": bot}, "on_behalf_of": alice, "data": {}},
                {"actor": {"kind": "share_link", "id": bob}, "on_behalf_of": null},
                {"grantee": {"type": "user", "id": bob, "email": "stale@x"}, "role": "view"},
                {"node": {"tenant_id": t2.0, "id": Uuid::now_v7()},
                 "shared_by": {"actor": {"kind": "user", "id": carol}, "on_behalf_of": null}},
                {"node": {"tenant_id": t2.0, "id": Uuid::now_v7()},
                 "shared_by": {"actor": {"kind": "user", "id": dave}, "on_behalf_of": null}},
                {"kind": "folder", "id": Uuid::now_v7()}
            ]
        });
        let mut seen = Vec::new();
        walk(&v, &positions, Some(t1), &mut |t, id| seen.push((t, id)));
        assert_eq!(seen.len(), 5, "{seen:?}");
        assert!(seen.contains(&(t2, carol)) && seen.contains(&(t2, dave)));
        let names = HashMap::from([
            ((t1, alice), name("Alice", Some("alice@x"))),
            ((t1, bot), name("Kadri Kask", None)),
            ((t1, bob), name("Bob", Some("bob@x"))),
            ((t2, dave), name("Dave", Some("dave@x"))),
            // Carol is known only in t1; the item of t2 must not take that name.
            ((t1, carol), name("Carol of t1", None)),
        ]);
        fill(&mut v, &positions, Some(t1), &names, false);
        let i = &v["items"];
        assert_eq!(i[0]["actor"]["display_name"], "Kadri Kask");
        assert_eq!(i[0]["on_behalf_of_display_name"], "Alice");
        assert!(i[1]["actor"].get("display_name").is_none());
        assert_eq!(i[2]["grantee"]["display_name"], "Bob");
        assert!(i[2]["grantee"].get("email").is_none());
        assert!(
            i[3]["shared_by"]["actor"].get("display_name").is_none(),
            "{}",
            i[3]
        );
        assert_eq!(i[4]["shared_by"]["actor"]["display_name"], "Dave");
        assert!(i[5].get("display_name").is_none());
        fill(&mut v, &positions, Some(t1), &names, true);
        assert_eq!(v["items"][2]["grantee"]["email"], "bob@x");
    }

    /// Principal-shaped JSON that the contract does not type as a principal: the rows of a query
    /// (backlog S-3), the manifest of a database, the `data` of an audit entry (backlog
    /// 2026-11-09 S-11). Also with `on_behalf_of`, a grantee or the `tenant_id` of another
    /// tenant, nothing in it is named, and it comes back unchanged; the typed fields next to it
    /// are named.
    #[test]
    fn names_go_only_where_the_contract_types_a_principal() {
        let (t1, t2) = (TenantId(Uuid::now_v7()), TenantId(Uuid::now_v7()));
        let [alice, bob, carol] = [(); 3].map(|_| Uuid::now_v7());
        let shaped = json!([
            {"kind": "user", "id": alice},
            {"actor": {"kind": "user", "id": alice}, "on_behalf_of": alice},
            {"grantee": {"type": "user", "id": alice}},
            {"tenant_id": t2.0, "kind": "user", "id": carol},
            {"node": {"tenant_id": t2.0}, "actor": {"kind": "user", "id": carol}},
            [{"kind": "user", "id": alice}],
            {"$ruum": "literal", "value": {"kind": "user", "id": alice}},
        ]);
        let names = HashMap::from([
            ((t1, alice), name("Alice", Some("alice@x"))),
            ((t1, bob), name("Bob", None)),
            ((t2, carol), name("Carol of t2", None)),
        ]);
        let node = Uuid::now_v7();
        let ref_of = |id: Uuid| json!({"kind": "user", "id": id});
        let cases = [
            (
                "POST",
                format!("/v1/databases/{node}/query"),
                json!({
                    "query_id": Uuid::now_v7(), "mode": "read", "columns": [],
                    "rows": [shaped.clone()], "row_count": 1, "content_trust": "untrusted",
                }),
                vec![],
            ),
            (
                "GET",
                format!("/v1/tenants/{}/audit", t1.0),
                json!({"items": [{
                    "id": Uuid::now_v7(), "action": "x", "actor": ref_of(bob), "on_behalf_of": null,
                    "target_type": "node", "target_id": node, "data": {"who": shaped.clone()},
                }]}),
                vec![(t1, bob)],
            ),
            (
                "GET",
                format!("/v1/databases/{node}"),
                json!({
                    "node": {"id": node, "created_by": {"actor": ref_of(bob), "on_behalf_of": null}},
                    "manifest": {"data_classes": {}, "x": shaped.clone()},
                }),
                vec![(t1, bob)],
            ),
        ];
        for (method, path, mut v, want) in cases {
            let positions = positions_for(method, &path).expect("an operation of the contract");
            let mut seen = Vec::new();
            walk(&v, positions, Some(t1), &mut |t, id| seen.push((t, id)));
            assert_eq!(seen, want, "{method} {path}");
            let before = v.to_string();
            fill(&mut v, positions, Some(t1), &names, true);
            let after = v.to_string();
            assert!(
                !after.contains("Alice") && !after.contains("Carol"),
                "{method} {path}: {after}"
            );
            if want.is_empty() {
                assert_eq!(after, before, "{method} {path}: unchanged");
            } else {
                assert!(after.contains("Bob"), "{method} {path}: {after}");
            }
        }
    }

    /// An answer of a path that the contract does not have gets no names; a literal segment of a
    /// path wins over a `{parameter}`.
    #[test]
    fn paths_match_the_contract() {
        assert!(positions_for("GET", "/v1/no-such-thing").is_none());
        assert!(positions_for("PATCH", "/v1/nodes/x/y/z/w").is_none());
        let p = positions_for("GET", &format!("/v1/nodes/{}", Uuid::now_v7())).unwrap();
        assert_eq!(p.path, "/v1/nodes/{node_id}");
        for o in &PRINCIPAL_PATHS.operations {
            let path = o.path.replace(['{', '}'], "");
            let concrete: String = path
                .split('/')
                .zip(o.path.split('/'))
                .map(|(c, t)| if t.starts_with('{') { "p" } else { c })
                .collect::<Vec<_>>()
                .join("/");
            let found = positions_for(&o.method, &concrete).unwrap();
            assert_eq!(found.method, o.method);
            assert_eq!(
                found.path.split('/').count(),
                o.path.split('/').count(),
                "{} {}",
                o.method,
                o.path
            );
        }
    }

    /// `principal_paths.json` is the output of `scripts/gen-principal-paths.mjs` for the current
    /// contract.
    #[test]
    fn principal_paths_are_up_to_date() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            PRINCIPAL_PATHS.openapi_sha256,
            hex::encode(Sha256::digest(
                include_str!("../../../contracts/openapi.yaml").as_bytes()
            )),
            "contracts/openapi.yaml changed: run `node crates/ruum-api/scripts/gen-principal-paths.mjs` \
             from the repository root"
        );
    }

    #[test]
    fn nothing_without_a_tenant() {
        let mut found = 0;
        let positions = Positions {
            refs: steps(&[&["actor"]]),
            ..Positions::default()
        };
        walk(
            &json!({"actor": {"kind": "user", "id": Uuid::now_v7()}}),
            &positions,
            None,
            &mut |_, _| found += 1,
        );
        assert_eq!(found, 0);
    }
}
