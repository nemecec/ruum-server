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

use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use ruum_core::tenants::PrincipalName;
use ruum_core::{Context, PrincipalKind, TenantId};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
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

/// The middleware: runs the request with a fresh scope, then fills the names into a JSON answer.
pub async fn fill_names(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let scope = Arc::new(Mutex::new(Scope::default()));
    let res = SCOPE.scope(scope.clone(), next.run(req)).await;
    let scope = scope.lock().map(|s| *s).unwrap_or_default();
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
    walk(&value, scope.tenant, &mut |t, id| {
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
    fill(&mut value, scope.tenant, &names, scope.grantee_emails);
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

fn walk(v: &Value, tenant: Option<TenantId>, found: &mut dyn FnMut(TenantId, Uuid)) {
    match v {
        Value::Array(items) => items.iter().for_each(|i| walk(i, tenant, found)),
        Value::Object(obj) => {
            let tenant = tenant_of(obj, tenant);
            if let Some(t) = tenant {
                if let Some(id) = principal_ref(obj) {
                    found(t, id);
                }
                if let Some(id) = uuid_of(obj.get("on_behalf_of")) {
                    found(t, id);
                }
                if let Some(id) = obj
                    .get("grantee")
                    .and_then(Value::as_object)
                    .and_then(user_grantee)
                {
                    found(t, id);
                }
            }
            obj.values().for_each(|c| walk(c, tenant, found));
        }
        _ => {}
    }
}

fn fill(
    v: &mut Value,
    tenant: Option<TenantId>,
    names: &HashMap<(TenantId, Uuid), PrincipalName>,
    grantee_emails: bool,
) {
    match v {
        Value::Array(items) => items
            .iter_mut()
            .for_each(|i| fill(i, tenant, names, grantee_emails)),
        Value::Object(obj) => {
            let tenant = tenant_of(obj, tenant);
            if let Some(t) = tenant {
                if let Some(n) = principal_ref(obj).and_then(|id| names.get(&(t, id))) {
                    obj.insert("display_name".into(), n.display_name.clone().into());
                }
                if let Some(n) = uuid_of(obj.get("on_behalf_of")).and_then(|id| names.get(&(t, id)))
                {
                    obj.insert(
                        "on_behalf_of_display_name".into(),
                        n.display_name.clone().into(),
                    );
                }
                if let Some(g) = obj.get_mut("grantee").and_then(Value::as_object_mut)
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
            }
            obj.values_mut()
                .for_each(|c| fill(c, tenant, names, grantee_emails));
        }
        _ => {}
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

    #[test]
    fn fills_refs_obo_and_grantees_in_their_tenant() {
        let (t1, t2) = (TenantId(Uuid::now_v7()), TenantId(Uuid::now_v7()));
        let [alice, bot, bob, carol, dave] = [(); 5].map(|_| Uuid::now_v7());
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
        walk(&v, Some(t1), &mut |t, id| seen.push((t, id)));
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
        fill(&mut v, Some(t1), &names, false);
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
        fill(&mut v, Some(t1), &names, true);
        assert_eq!(v["items"][2]["grantee"]["email"], "bob@x");
    }

    #[test]
    fn nothing_without_a_tenant() {
        let mut found = 0;
        walk(
            &json!({"actor": {"kind": "user", "id": Uuid::now_v7()}}),
            None,
            &mut |_, _| found += 1,
        );
        assert_eq!(found, 0);
    }
}
