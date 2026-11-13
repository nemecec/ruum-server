//! The runtime API and the internal gateway (cards R-3 and G-3): every case is in
//! [`super::PENDING`] with its card until a [`super::RuntimeApiWorld`] exists.

use super::token::{decode_unverified, delegated_schema_problems};
use super::{Binding, BindingKind, Echo, Env, Fail, Req, RtCtx, RuntimeCase};
use crate::{gw_check, gw_fail};
use serde_json::json;

macro_rules! case {
    ($name:expr, $rule:expr, $source:expr, [$($op:expr),* $(,)?], |$cx:ident| $body:block) => {
        RuntimeCase {
            name: $name,
            rule: $rule,
            source: $source,
            ops: &[$($op),*],
            run: |$cx: RtCtx| -> futures::future::BoxFuture<'static, super::CaseResult> {
                Box::pin(async move $body)
            },
        }
    };
}

fn bearer(req: Req, token: &str) -> Req {
    req.header("Authorization", &format!("Bearer {token}"))
}

fn file_binding(name: &str, obo: bool) -> Binding {
    Binding {
        name: name.into(),
        kind: BindingKind::File(b"T6-BOUND-FILE".to_vec()),
        on_behalf_of_user: obo,
        delegate: false,
    }
}

fn node_of(app: &super::RtApp, binding: &str) -> Result<String, Fail> {
    app.bound_nodes
        .iter()
        .find(|(b, _)| b == binding)
        .map(|(_, n)| n.clone())
        .ok_or_else(|| Fail::Failed(format!("the world gave no node for binding {binding}")))
}

/// `POST /v1/token/exchange` with the workload token of the caller.
async fn exchange(cx: &RtCtx, workload: &str, user_token: &str) -> Result<super::Resp, Fail> {
    let req = bearer(
        Req::post(&cx.rt.internal_host, "/v1/token/exchange")
            .json(&json!({ "user_token": user_token })),
        workload,
    );
    cx.send(req).await
}

pub fn cases() -> Vec<RuntimeCase> {
    vec![
        case!(
            "get_app_self_needs_a_workload_token",
            "GET /v1/self answers the app with its workload token and 401 without it",
            "contracts/apps/runtime.openapi.yaml /v1/self",
            ["getAppSelf"],
            |cx| {
                let app = cx.world.seed_app("acme", "garden", &[]).await?;
                let tok = cx.workload_token(&app, Env::Live).await?;
                let h = cx.rt.internal_host.clone();
                let resp = cx
                    .send(bearer(Req::get(&h, "/v1/self"), tok.expose()))
                    .await?;
                gw_check!(resp.status == 200, "GET /v1/self: {}", resp.status);
                gw_check!(
                    resp.text().contains(&app.id),
                    "GET /v1/self does not name the app"
                );
                let resp = cx.send(Req::get(&h, "/v1/self")).await?;
                gw_check!(
                    resp.status == 401,
                    "GET /v1/self without a token: {}",
                    resp.status
                );
                Ok(())
            }
        ),
        case!(
            "list_app_self_users_names_role_holders",
            "GET /v1/self/users?role= lists holders as pairwise IDs; a role not in the live \
             manifest is 404",
            "contracts/apps/runtime.openapi.yaml /v1/self/users",
            ["listAppSelfUsers"],
            |cx| {
                let app = cx.world.seed_app("acme", "garden", &[]).await?;
                let tok = cx.workload_token(&app, Env::Live).await?;
                let h = cx.rt.internal_host.clone();
                let resp = cx
                    .send(bearer(
                        Req::get(&h, "/v1/self/users?role=nosuchrole"),
                        tok.expose(),
                    ))
                    .await?;
                gw_check!(resp.status == 404, "an unknown role: {}", resp.status);
                Ok(())
            }
        ),
        case!(
            "exchange_refuses_a_preview_token_for_the_live_app",
            "exchangeAppUserToken refuses (401) a preview's user token sent by the live app, and \
             a live token sent by the preview",
            "contracts/apps/runtime.openapi.yaml /v1/token/exchange; decision 0006-10; STEP3 \
             \"How an app runs\" rule 4",
            ["exchangeAppUserToken"],
            |cx| {
                let app = cx
                    .world
                    .seed_app("acme", "garden", &[file_binding("doc", true)])
                    .await?;
                let live_w = cx.workload_token(&app, Env::Live).await?;
                let prev_w = cx.workload_token(&app, Env::Preview).await?;
                let live_u = cx.user_token(&app, Env::Live, "user").await?;
                let prev_u = cx.user_token(&app, Env::Preview, "user").await?;
                let r = exchange(&cx, live_w.expose(), prev_u.expose()).await?;
                gw_check!(
                    r.status == 401,
                    "the live app exchanged a preview token: {}",
                    r.status
                );
                let r = exchange(&cx, prev_w.expose(), live_u.expose()).await?;
                gw_check!(
                    r.status == 401,
                    "the preview exchanged a live token: {}",
                    r.status
                );
                Ok(())
            }
        ),
        case!(
            "exchange_refuses_a_token_of_another_app",
            "exchangeAppUserToken refuses (401) a user token whose aud is another app",
            "contracts/apps/runtime.openapi.yaml /v1/token/exchange; decision 0006-10",
            ["exchangeAppUserToken"],
            |cx| {
                let a = cx
                    .world
                    .seed_app("acme", "garden", &[file_binding("doc", true)])
                    .await?;
                let b = cx
                    .world
                    .seed_app("acme", "orchard", &[file_binding("doc", true)])
                    .await?;
                let b_w = cx.workload_token(&b, Env::Live).await?;
                let a_u = cx.user_token(&a, Env::Live, "user").await?;
                let r = exchange(&cx, b_w.expose(), a_u.expose()).await?;
                gw_check!(
                    r.status == 401,
                    "app B exchanged app A's token: {}",
                    r.status
                );
                Ok(())
            }
        ),
        case!(
            "exchange_refuses_an_actor_without_delegate",
            "exchangeAppUserToken refuses (403) a token whose actor is not user or agent unless \
             the binding has delegate",
            "contracts/apps/runtime.openapi.yaml /v1/token/exchange; decision 0006-10",
            ["exchangeAppUserToken"],
            |cx| {
                let app = cx
                    .world
                    .seed_app("acme", "garden", &[file_binding("doc", true)])
                    .await?;
                let w = cx.workload_token(&app, Env::Live).await?;
                for actor in ["app", "partner", "anonymous"] {
                    let u = cx.user_token(&app, Env::Live, actor).await?;
                    let r = exchange(&cx, w.expose(), u.expose()).await?;
                    gw_check!(r.status == 403, "actor {actor} was exchanged: {}", r.status);
                }
                Ok(())
            }
        ),
        case!(
            "exchange_gives_a_delegated_token",
            "exchangeAppUserToken gives a delegated token (typ app_delegated, aud ruum.internal, \
             at most 300 s, the bindings with on_behalf_of_user) that matches \
             delegated-token.schema.json",
            "contracts/apps/runtime.openapi.yaml /v1/token/exchange; \
             contracts/apps/delegated-token.schema.json",
            ["exchangeAppUserToken"],
            |cx| {
                let app = cx
                    .world
                    .seed_app(
                        "acme",
                        "garden",
                        &[file_binding("doc", true), file_binding("raw", false)],
                    )
                    .await?;
                let w = cx.workload_token(&app, Env::Live).await?;
                let u = cx.user_token(&app, Env::Live, "user").await?;
                let r = exchange(&cx, w.expose(), u.expose()).await?;
                gw_check!(r.status == 200, "the exchange answered {}", r.status);
                let v = r.json().unwrap_or_default();
                let Some(t) = v["access_token"].as_str() else {
                    gw_fail!("the exchange gave no access_token");
                };
                cx.secret(t);
                let (_, c) = decode_unverified(t).map_err(Fail::Failed)?;
                let p = delegated_schema_problems(&c);
                gw_check!(
                    p.is_empty(),
                    "the delegated token's claims: {}",
                    p.join("; ")
                );
                gw_check!(
                    c["typ"] == "app_delegated" && c["aud"] == "ruum.internal",
                    "typ or aud"
                );
                let life = c["exp"].as_i64().unwrap_or(0) - c["iat"].as_i64().unwrap_or(0);
                gw_check!(
                    life > 0 && life <= 300,
                    "the delegated token lives {life} s"
                );
                gw_check!(
                    c["bindings"] == json!(["doc"]),
                    "bindings: {}",
                    c["bindings"]
                );
                Ok(())
            }
        ),
        case!(
            "bound_content_only",
            "readBoundContent reads a bound file and refuses a node outside every binding \
             (403 or 404)",
            "contracts/apps/runtime.openapi.yaml /v1/nodes/{node_id}/content; STEP3 \"How an \
             app runs\" rule 5",
            ["readBoundContent"],
            |cx| {
                let app = cx
                    .world
                    .seed_app("acme", "garden", &[file_binding("doc", false)])
                    .await?;
                let w = cx.workload_token(&app, Env::Live).await?;
                let h = cx.rt.internal_host.clone();
                let n = node_of(&app, "doc")?;
                let r = cx
                    .send(bearer(
                        Req::get(&h, &format!("/v1/nodes/{n}/content")),
                        w.expose(),
                    ))
                    .await?;
                gw_check!(
                    r.status == 200 && r.body == b"T6-BOUND-FILE",
                    "the bound file: {}",
                    r.status
                );
                let other = cx.world.seed_unbound_node("acme").await?;
                let r = cx
                    .send(bearer(
                        Req::get(&h, &format!("/v1/nodes/{other}/content")),
                        w.expose(),
                    ))
                    .await?;
                gw_check!(
                    matches!(r.status, 403 | 404),
                    "an unbound node: {}",
                    r.status
                );
                Ok(())
            }
        ),
        case!(
            "bound_children_only",
            "listBoundChildren lists a bound folder and refuses a folder outside every binding",
            "contracts/apps/runtime.openapi.yaml /v1/nodes/{node_id}/children",
            ["listBoundChildren"],
            |cx| {
                let b = Binding {
                    name: "photos".into(),
                    kind: BindingKind::Folder(vec!["a.jpg".into(), "b.jpg".into()]),
                    on_behalf_of_user: false,
                    delegate: false,
                };
                let app = cx.world.seed_app("acme", "garden", &[b]).await?;
                let w = cx.workload_token(&app, Env::Live).await?;
                let h = cx.rt.internal_host.clone();
                let n = node_of(&app, "photos")?;
                let r = cx
                    .send(bearer(
                        Req::get(&h, &format!("/v1/nodes/{n}/children")),
                        w.expose(),
                    ))
                    .await?;
                gw_check!(r.status == 200, "the bound folder: {}", r.status);
                let t = r.text();
                gw_check!(
                    t.contains("a.jpg") && t.contains("b.jpg"),
                    "the children are missing"
                );
                let other = cx.world.seed_unbound_node("acme").await?;
                let r = cx
                    .send(bearer(
                        Req::get(&h, &format!("/v1/nodes/{other}/children")),
                        w.expose(),
                    ))
                    .await?;
                gw_check!(
                    matches!(r.status, 403 | 404),
                    "an unbound folder: {}",
                    r.status
                );
                Ok(())
            }
        ),
        case!(
            "publish_app_event_needs_the_topic",
            "publishAppEvent refuses (403) a topic that the live manifest does not declare",
            "contracts/apps/runtime.openapi.yaml /v1/events/{topic}",
            ["publishAppEvent"],
            |cx| {
                let app = cx.world.seed_app("acme", "garden", &[]).await?;
                let w = cx.workload_token(&app, Env::Live).await?;
                let h = cx.rt.internal_host.clone();
                let r = cx
                    .send(bearer(
                        Req::post(&h, "/v1/events/t6.undeclared").json(&json!({"x": 1})),
                        w.expose(),
                    ))
                    .await?;
                gw_check!(r.status == 403, "an undeclared topic: {}", r.status);
                Ok(())
            }
        ),
        case!(
            "app_tokens_never_work_at_the_rest_api",
            "A workload token and a user token are refused by the REST API (401)",
            "contracts/apps/README.md \"Rules\"; decision 0006-10",
            ["getAppSelf"],
            |cx| {
                let app = cx.world.seed_app("acme", "garden", &[]).await?;
                let w = cx.workload_token(&app, Env::Live).await?;
                let u = cx.user_token(&app, Env::Live, "user").await?;
                for (what, t) in [("workload", &w), ("user", &u)] {
                    let st = cx.world.rest_api_status(t.expose()).await?;
                    gw_check!(st == 401, "the REST API answered {st} to a {what} token");
                }
                Ok(())
            }
        ),
        case!(
            "the_internal_gateway_removes_the_workload_token",
            "An app-to-app call through the internal gateway reaches app B without Authorization \
             and without app A's workload token in any header; B sees actor app in Ruum-User",
            "contracts/apps/runtime.openapi.yaml \"The app's credential\"; decision 0006-10; \
             STEP3 \"How an app runs\" rule 4; STEP3 card T-6 \"Done when\"",
            [],
            |cx| {
                let echo = Echo::start().await;
                let b = cx.world.seed_app("acme", "orchard", &[]).await?;
                cx.world.set_upstream(&b, &echo.url).await?;
                let a = cx
                    .world
                    .seed_app(
                        "acme",
                        "garden",
                        &[Binding {
                            name: "orders".into(),
                            kind: BindingKind::App {
                                app_slug: "orchard".into(),
                                api: "api".into(),
                            },
                            on_behalf_of_user: false,
                            delegate: false,
                        }],
                    )
                    .await?;
                let w = cx.workload_token(&a, Env::Live).await?;
                let host = format!("orders.api.{}", cx.rt.internal_host);
                let r = cx
                    .send(bearer(Req::get(&host, "/items"), w.expose()))
                    .await?;
                gw_check!(r.status == 200, "the app-to-app call: {}", r.status);
                let seen = echo.seen();
                let Some(last) = seen.last() else {
                    gw_fail!("app B got no request");
                };
                gw_check!(
                    last.header_all("authorization").is_empty(),
                    "app B got an Authorization header"
                );
                for (n, v) in &last.headers {
                    gw_check!(
                        !v.contains(w.expose()),
                        "app B got A's workload token in {n}"
                    );
                }
                let ru = last.header_all("ruum-user");
                let actor = ru
                    .first()
                    .and_then(|t| decode_unverified(t).ok())
                    .map(|(_, c)| c["actor"].clone());
                gw_check!(
                    actor.as_ref().map(|a| a == "app").unwrap_or(false),
                    "app B should see actor app, got {actor:?}"
                );
                Ok(())
            }
        ),
        case!(
            "a_chain_longer_than_three_hops_is_refused",
            "A delegation chain longer than 3 hops is refused",
            "contracts/apps/user-token.schema.json chain (maxItems 3); design 9.10; \
             contracts/apps/runtime.openapi.yaml /v1/token/exchange",
            ["exchangeAppUserToken"],
            |_cx| {
                Err(Fail::Failed(
                    "planned case body: G-3 writes it when it removes the PENDING line. Steps: \
                     four apps bound in a row with delegate; each exchanges and calls the next; \
                     the fourth hop is refused (403)"
                        .into(),
                ))
            }
        ),
    ]
}
