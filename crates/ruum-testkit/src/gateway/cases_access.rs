//! The access check (cases 6, 7, 8, 13, 15 and 18 of card T-6): `use` on live hosts,
//! `open_preview` on previews, removed rights, agent tokens, host rules and public apps.

use super::token::{Expect, verify_user_token};
use super::{
    Access, AgentAudience, Ctx, Echo, Env, Fail, GatewayCase, Jar, Relation, Req, Resp,
    SIGN_IN_OR_REFUSED, echoed, echoed_header, expect_no_session, expect_refusal, files, is_echo,
    page, poll_until, query_param, same_origin_path, scene,
};
use crate::{gw_check, gw_fail};
use std::time::Duration;
use tokio::time::Instant;

macro_rules! case {
    ($name:expr, $rule:expr, $source:expr, [$($op:expr),* $(,)?], |$cx:ident| $body:block) => {
        GatewayCase {
            name: $name,
            rule: $rule,
            source: $source,
            ops: &[$($op),*],
            run: |$cx: Ctx| -> futures::future::BoxFuture<'static, super::CaseResult> {
                Box::pin(async move $body)
            },
        }
    };
}

/// The bound of "a removed right stops working" (card G-1a, decision 0006 G).
pub const REVOKE_BOUND: Duration = Duration::from_secs(5);

/// A redirect to the gateway's own sign-in start on the same host, whose `return_to` is
/// `return_to` (the target the visitor asked for).
fn expect_sign_in_redirect(
    label: &str,
    resp: &Resp,
    host: &str,
    return_to: &str,
) -> super::CaseResult {
    gw_check!(
        !is_echo(resp) && resp.is_redirect(),
        "{label}: expected a redirect to /.ruum/v1/auth/start, got {} {}",
        resp.status,
        resp.short_body()
    );
    let loc = resp.header("location").unwrap_or("");
    let path = same_origin_path(loc, host);
    gw_check!(
        path.as_deref()
            .map(|p| p.starts_with("/.ruum/v1/auth/start?"))
            .unwrap_or(false),
        "{label}: the redirect goes to {loc:?}, expected /.ruum/v1/auth/start?return_to=... on \
         the same host"
    );
    let rt = query_param(loc, "return_to");
    gw_check!(
        rt.as_deref() == Some(return_to),
        "{label}: the sign-in redirect's return_to is {rt:?}, expected {return_to:?}"
    );
    Ok(())
}

/// The claims of the one `Ruum-User` token of an echoed request, verified with the gateway's key
/// set for `expect`.
fn verified_token(
    cx: &Ctx,
    label: &str,
    v: &serde_json::Value,
    expect: &Expect,
) -> Result<serde_json::Value, Fail> {
    let t = echoed_header(v, "ruum-user");
    gw_check!(
        t.len() == 1,
        "{label}: the app got {} Ruum-User headers, expected one",
        t.len()
    );
    cx.secret(&t[0]);
    verify_user_token(&t[0], &cx.gw.user_token_jwks, expect)
        .map_err(|e| Fail::Failed(format!("{label}: the user token does not verify: {e}")))
}

/// A preview of `app` with an echo upstream.
async fn preview_with_echo(cx: &Ctx, app: &super::App) -> Result<(super::Deployment, Echo), Fail> {
    let prev = cx
        .world
        .seed_deployment(
            app,
            Env::Preview,
            &files(&[("index.html", &page("preview"))]),
            None,
        )
        .await?;
    let echo = Echo::start().await;
    cx.world.set_upstream(&prev, &echo.url).await?;
    Ok((prev, echo))
}

pub fn cases() -> Vec<GatewayCase> {
    vec![
        case!(
            "use_is_checked_on_a_live_host",
            "On a live host of an access: platform app a visitor without a session is sent to \
             /.ruum/v1/auth/start with return_to; a signed-in visitor without use gets the 403 \
             page and the app sees nothing; view gives use",
            "gateway.openapi.yaml \"The check of every request\" 2; STEP3 \"How an app runs\" \
             rule 3; schema.zed permission use",
            ["appAuthStart"],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::get(&s.host, "/private?q=1");
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_sign_in_redirect(&label, &resp, &s.host, "/private?q=1")?;
                // A member without any right on the app.
                let stranger = cx.world.seed_user(&s.tenant, "toomas").await?;
                let (mut jar, _) = cx.sign_in(&stranger, &s.host).await?;
                let req = Req::get(&s.host, "/private");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                expect_refusal(&label, &resp, &[403])?;
                let body = resp.text().to_ascii_lowercase();
                gw_check!(
                    body.contains("ask") && body.contains("access"),
                    "{label}: the 403 page does not tell the visitor to ask for access"
                );
                gw_check!(
                    s.echo().hits() == 0,
                    "{label}: the app got {} requests before any visitor with use",
                    s.echo().hits()
                );
                // The member with view.
                let req = Req::get(&s.host, "/private");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                echoed(&label, &resp)?;
                Ok(())
            }
        ),
        case!(
            "an_active_app_role_gives_use",
            "A visitor whose only right is an active app role passes the use check",
            "gateway.openapi.yaml \"The check of every request\" 2 (use = view or any app \
             role); design 9.3; card B-2",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let member = cx.world.seed_user(&s.tenant, "kati").await?;
                cx.world
                    .grant(&member, &s.app, &Relation::Role("member".into()))
                    .await?;
                let (mut jar, _) = cx.sign_in(&member, &s.host).await?;
                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                echoed(&label, &resp)?;
                Ok(())
            }
        ),
        case!(
            "open_preview_is_checked_on_a_preview_host",
            "A preview host needs open_preview (edit today): edit passes, view is refused and \
             the app sees nothing",
            "gateway.openapi.yaml \"The check of every request\" 2; STEP3 \"How an app runs\" \
             rule 3; decision 0006-5",
            [],
            |cx| {
                let s = scene(&cx, false).await?;
                let (prev, echo) = preview_with_echo(&cx, &s.app).await?;
                let host = cx.preview_host(&s.app, &prev);
                // s.user has view only.
                let (mut jar, _) = cx.sign_in(&s.user, &host).await?;
                let req = Req::get(&host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                expect_refusal(&label, &resp, &[403])?;
                gw_check!(
                    echo.hits() == 0,
                    "{label}: the preview got a request from a viewer"
                );
                let editor = cx.world.seed_user(&s.tenant, "editor").await?;
                cx.world.grant(&editor, &s.app, &Relation::Edit).await?;
                let (mut jar, _) = cx.sign_in(&editor, &host).await?;
                let req = Req::get(&host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                echoed(&label, &resp)?;
                Ok(())
            }
        ),
        case!(
            "a_removed_right_stops_within_5_seconds",
            "After view is removed, the visitor is refused within 5 seconds (bounded poll; a \
             pass needs the gateway's refusal inside the bound)",
            "card G-1a \"Done when\"; user-token.schema.json description (made for each \
             request or cached at most 5 s)",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                echoed(&label, &resp)?;
                cx.world.revoke(&s.user, &s.app, &Relation::View).await?;
                let since = Instant::now();
                let label = format!("GET {}/ after view was removed", s.host);
                poll_until(
                    &label,
                    since,
                    REVOKE_BOUND,
                    Duration::from_millis(200),
                    || {
                        let cx = cx.clone();
                        let mut jar = s.jar.clone();
                        let host = s.host.clone();
                        let label = label.clone();
                        async move {
                            let resp = cx.send_jar(&mut jar, Req::get(&host, "/")).await?;
                            if is_echo(&resp) {
                                return Ok(false);
                            }
                            expect_refusal(&label, &resp, &[403])?;
                            Ok(true)
                        }
                    },
                )
                .await
            }
        ),
        case!(
            "a_removed_role_stops_within_5_seconds",
            "After the only app role is removed, the visitor is refused within 5 seconds",
            "card G-1a \"Done when\"; STEP3 card T-6 \"Done when\"; card B-2",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let member = cx.world.seed_user(&s.tenant, "kati").await?;
                let role = Relation::Role("member".into());
                cx.world.grant(&member, &s.app, &role).await?;
                let (jar, _) = cx.sign_in(&member, &s.host).await?;
                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar.clone(), req).await?;
                echoed(&label, &resp)?;
                cx.world.revoke(&member, &s.app, &role).await?;
                let since = Instant::now();
                let label = format!("GET {}/ after the role was removed", s.host);
                poll_until(
                    &label,
                    since,
                    REVOKE_BOUND,
                    Duration::from_millis(200),
                    || {
                        let cx = cx.clone();
                        let mut jar = jar.clone();
                        let host = s.host.clone();
                        let label = label.clone();
                        async move {
                            let resp = cx.send_jar(&mut jar, Req::get(&host, "/")).await?;
                            if is_echo(&resp) {
                                return Ok(false);
                            }
                            expect_refusal(&label, &resp, &[403])?;
                            Ok(true)
                        }
                    },
                )
                .await
            }
        ),
        case!(
            "agent_tokens_reach_previews_only",
            "An agent token with the audience ruumapps is refused on a preview when its user has \
             no open_preview, and reaches it with open_preview (the app sees actor agent and \
             on_behalf_of the user's pairwise ID, never the platform user ID); a live host \
             refuses it with 403; an agent token with the API audience is refused on every host",
            "decision 0006-10; gateway.openapi.yaml \"The check of every request\" 2 and \
             securitySchemes agentToken; STEP3 card T-6 \"Done when\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let (prev, echo) = preview_with_echo(&cx, &s.app).await?;
                let phost = cx.preview_host(&s.app, &prev);
                // The agent of a member with view only: no open_preview, so refused.
                let viewer = cx.world.seed_user(&s.tenant, "viewer").await?;
                cx.world.grant(&viewer, &s.app, &Relation::View).await?;
                let vtok = cx.agent_token(&viewer, AgentAudience::Ruumapps).await?;
                let req = Req::get(&phost, "/")
                    .header("Authorization", &format!("Bearer {}", vtok.expose()));
                let label = format!("{} with the agent token of a viewer", req.label());
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[403])?;
                gw_check!(
                    echo.hits() == 0,
                    "{label}: the preview got the request of an agent without open_preview"
                );
                cx.world.grant(&s.user, &s.app, &Relation::Edit).await?;
                let tok = cx.agent_token(&s.user, AgentAudience::Ruumapps).await?;
                let bearer = format!("Bearer {}", tok.expose());
                let req = Req::get(&phost, "/").header("Authorization", &bearer);
                let label = req.label();
                let resp = cx.send(req).await?;
                let v = echoed(&label, &resp)?;
                let c = verified_token(
                    &cx,
                    &label,
                    &v,
                    &Expect::preview(&cx.gw.issuer, &s.app.id, &prev.id),
                )?;
                gw_check!(
                    c["actor"] == "agent",
                    "{label}: the preview should see actor agent, got {}",
                    c["actor"]
                );
                let obo = c["on_behalf_of"].as_str().unwrap_or("");
                gw_check!(
                    obo.starts_with("pairwise:") && !obo.contains(&s.user.id),
                    "{label}: on_behalf_of must be the user's pairwise ID (pairwise:...), never \
                     the platform user ID"
                );
                let live_hits = s.echo().hits();
                let req = Req::get(&s.host, "/").header("Authorization", &bearer);
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[403])?;
                gw_check!(
                    s.echo().hits() == live_hits,
                    "{label}: the live app got the agent's request"
                );
                let api = cx.agent_token(&s.user, AgentAudience::Api).await?;
                let api_bearer = format!("Bearer {}", api.expose());
                let prev_hits = echo.hits();
                for host in [&phost, &s.host] {
                    let req = Req::get(host, "/").header("Authorization", &api_bearer);
                    let label = format!("{} with an API-audience agent token", req.label());
                    let resp = cx.send(req).await?;
                    expect_refusal(&label, &resp, &[401, 403])?;
                }
                gw_check!(
                    echo.hits() == prev_hits && s.echo().hits() == live_hits,
                    "an agent token with the API audience reached an app"
                );
                Ok(())
            }
        ),
        case!(
            "host_rules_answer_the_gateways_404",
            "An unknown host, a reserved tenant or app slug, a malformed label (--, xn--, a dash \
             at an end, an upper-case short ID), an extra label, a host outside the apps domain \
             and a preview short_id of another app answer the gateway's 404; an app that exists \
             but that the visitor may not see answers the sign-in redirect or 403, with a body \
             that names neither the tenant nor the app",
            "decision 0006-16; STEP3 \"Host names and slugs\" and \"System lookups\"; \
             gateway.openapi.yaml \"The check of every request\" 1",
            [],
            |cx| {
                let s = super::scene_with(&cx, "orchidco", "zinniaapp", true).await?;
                let other = cx
                    .world
                    .seed_app(&s.tenant, "shedapp", Access::Platform)
                    .await?;
                let other_prev = cx
                    .world
                    .seed_deployment(
                        &other,
                        Env::Preview,
                        &files(&[("index.html", &page("o"))]),
                        None,
                    )
                    .await?;
                let d = cx.gw.apps_domain.clone();
                let hosts = vec![
                    format!("nosuchapp.orchidco.{d}"),
                    format!("zinniaapp.nosuchtenant.{d}"),
                    format!("zinniaapp.content.{d}"),
                    format!("zinniaapp.www.{d}"),
                    format!("admin.orchidco.{d}"),
                    format!("api.orchidco.{d}"),
                    format!("zi.orchidco.{d}"),
                    format!("zinnia--app.orchidco.{d}"),
                    format!("xn--zinniaapp.orchidco.{d}"),
                    format!("-zinniaapp.orchidco.{d}"),
                    format!("zinniaapp-.orchidco.{d}"),
                    format!("ABCDEFGH--zinniaapp.orchidco.{d}"),
                    format!("{}--zinniaapp.orchidco.{d}", other_prev.short_id),
                    format!("x.zinniaapp.orchidco.{d}"),
                    format!("orchidco.{d}"),
                    d.clone(),
                    "zinniaapp.orchidco.example.com".to_string(),
                    format!("zinniaapp.orchidco.{d}.example.com"),
                ];
                for host in hosts {
                    let req = Req::get(&host, "/");
                    let label = req.label();
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    expect_refusal(&label, &resp, &[404])?;
                }
                gw_check!(
                    s.echo().hits() == 0,
                    "a host rule let a request reach the app"
                );
                // Exists, but not for this visitor.
                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                let stranger = cx.world.seed_user(&s.tenant, "stranger").await?;
                let (mut jar, _) = cx.sign_in(&stranger, &s.host).await?;
                let req = Req::get(&s.host, "/");
                let label2 = req.label();
                let resp2 = cx.send_jar(&mut jar, req).await?;
                expect_refusal(&label2, &resp2, &[403])?;
                for (l, r) in [(&label, &resp), (&label2, &resp2)] {
                    let body = r.text().to_ascii_lowercase();
                    gw_check!(
                        !body.contains("zinniaapp") && !body.contains("orchidco"),
                        "{l}: the refusal's body names the tenant or the app"
                    );
                }
                Ok(())
            }
        ),
        case!(
            "public_apps_pass_while_approved",
            "access: public passes without sign-in (actor anonymous) and a signed-in visitor \
             still gets a user token (actor user); when the tenant's public approval is \
             withdrawn, anonymous requests are refused within the cache bound of 5 s",
            "gateway.openapi.yaml \"The check of every request\" 2; backlog S-33; card G-1a \
             \"The access check\"",
            [],
            |cx| {
                let w = &cx.world;
                let tenant = w.seed_tenant("acme").await?;
                w.set_public_approval(&tenant, true).await?;
                let user = w.seed_user(&tenant, "mari").await?;
                let app = w.seed_app(&tenant, "garden", Access::Public).await?;
                let live = w
                    .seed_deployment(
                        &app,
                        Env::Live,
                        &files(&[("index.html", &page("pub"))]),
                        None,
                    )
                    .await?;
                w.set_live(&app, &live).await?;
                let echo = Echo::start().await;
                w.set_upstream(&live, &echo.url).await?;
                let host = cx.live_host(&app);
                let expect = Expect::live(&cx.gw.issuer, &app.id);
                let claims_of = |label: &str, resp: &Resp| -> Result<serde_json::Value, Fail> {
                    let v = echoed(label, resp)?;
                    if echoed_header(&v, "ruum-user").is_empty() {
                        gw_fail!("{label}: no Ruum-User on a public app");
                    }
                    verified_token(&cx, label, &v, &expect)
                };
                let req = Req::get(&host, "/");
                let label = req.label();
                let resp = cx.send(req).await?;
                let c = claims_of(&label, &resp)?;
                gw_check!(
                    c["actor"] == "anonymous",
                    "{label}: anonymous visitor has actor {}",
                    c["actor"]
                );
                gw_check!(
                    c["sub"]
                        .as_str()
                        .map(|x| x.starts_with("anonymous:"))
                        .unwrap_or(false),
                    "{label}: the sub of an anonymous visitor must start with anonymous:"
                );
                let (mut jar, _) = cx.sign_in(&user, &host).await?;
                let req = Req::get(&host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                let c = claims_of(&label, &resp)?;
                gw_check!(
                    c["actor"] == "user",
                    "{label}: signed-in visitor has actor {}",
                    c["actor"]
                );
                w.set_public_approval(&tenant, false).await?;
                let since = Instant::now();
                let label = format!("GET {host}/ after the public approval was withdrawn");
                poll_until(
                    &label,
                    since,
                    REVOKE_BOUND,
                    Duration::from_millis(200),
                    || {
                        let cx = cx.clone();
                        let host = host.clone();
                        let label = label.clone();
                        async move {
                            let resp = cx.send(Req::get(&host, "/")).await?;
                            if is_echo(&resp) {
                                return Ok(false);
                            }
                            expect_refusal(&label, &resp, &[302, 303, 307, 401, 403, 404, 503])?;
                            Ok(true)
                        }
                    },
                )
                .await
            }
        ),
        case!(
            "sign_in_keeps_return_to_on_the_origin",
            "A sign-in started with a return_to that is not a path on this origin (//host, a \
             scheme, a backslash or an encoded form) is refused at the start (400) or ends with a \
             callback whose Location is a path on the same origin",
            "gateway.openapi.yaml /.ruum/v1/auth/start (return_to pattern); card G-1a \"Sign-in \
             on the app origin\" (the state is bound to the host and return_to)",
            ["appAuthStart", "appAuthCallback"],
            |cx| {
                let s = scene(&cx, false).await?;
                for rt in [
                    "//t6evil.example/x",
                    "https://t6evil.example/",
                    "/\\t6evil.example",
                    "%2F%2Ft6evil.example",
                    "/%2F%2Ft6evil.example",
                    "%2F%5Ct6evil.example",
                    "javascript:alert(1)",
                ] {
                    let mut jar = Jar::default();
                    let start = cx.auth_start(&mut jar, &s.host, rt).await?;
                    let label = format!("the sign-in on {} with return_to={rt}", s.host);
                    gw_check!(!is_echo(&start), "{label}: the app answered a gateway path");
                    if start.status == 400 {
                        continue;
                    }
                    let cb = cx.authorize(&s.user, &s.host, &start).await?;
                    let resp = cx.send_jar(&mut jar, Req::get(&s.host, &cb)).await?;
                    gw_check!(!is_echo(&resp), "{label}: the app answered the callback");
                    if resp.status == 400 {
                        expect_no_session(&label, &resp)?;
                        continue;
                    }
                    gw_check!(
                        resp.is_redirect(),
                        "{label}: the callback answered {}, expected 400 or a redirect",
                        resp.status
                    );
                    let loc = resp.header("location").unwrap_or("");
                    gw_check!(
                        same_origin_path(loc, &s.host).is_some(),
                        "{label}: the callback sends the browser to {loc:?}, off the origin"
                    );
                }
                Ok(())
            }
        ),
        case!(
            "sign_in_callback_refuses_a_bad_state",
            "/.ruum/v1/auth/callback with a made-up code and state answers 400 and sets no app \
             session",
            "gateway.openapi.yaml /.ruum/v1/auth/callback (400: a bad or expired state)",
            ["appAuthCallback"],
            |cx| {
                let s = scene(&cx, false).await?;
                let req = Req::get(
                    &s.host,
                    "/.ruum/v1/auth/callback?code=t6made-up&state=t6made-up",
                );
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[400])?;
                expect_no_session(&label, &resp)?;
                Ok(())
            }
        ),
        case!(
            "the_sign_in_code_is_bound_to_its_host_and_used_once",
            "The callback of a sign-in started on app A, sent to app B's host (the user has view \
             on both), is refused (400) and opens nothing; the same callback a second time is \
             refused (400, no session); when the start sets a state cookie, a callback without \
             it is refused",
            "card G-1a \"Sign-in on the app origin\" (a state bound to the host, a code used \
             once within 60 s); gateway.openapi.yaml /.ruum/v1/auth/callback (400)",
            ["appAuthStart", "appAuthCallback"],
            |cx| {
                let s = scene(&cx, true).await?;
                let w = &cx.world;
                let b = w.seed_app(&s.tenant, "orchard", Access::Platform).await?;
                let b_live = w
                    .seed_deployment(&b, Env::Live, &files(&[("index.html", &page("b"))]), None)
                    .await?;
                w.set_live(&b, &b_live).await?;
                let b_echo = Echo::start().await;
                w.set_upstream(&b_live, &b_echo.url).await?;
                w.grant(&s.user, &b, &Relation::View).await?;
                let b_host = cx.live_host(&b);

                // (a) A's callback on B's host, in a browser that started a sign-in on B.
                let mut a_jar = Jar::default();
                let start = cx.auth_start(&mut a_jar, &s.host, "/").await?;
                let a_cb = cx.authorize(&s.user, &s.host, &start).await?;
                let mut b_jar = Jar::default();
                cx.auth_start(&mut b_jar, &b_host, "/").await?;
                let req = Req::get(&b_host, &a_cb);
                let label = format!("GET {b_host} with the callback of a sign-in on app A");
                let resp = cx.send_jar(&mut b_jar, req).await?;
                expect_refusal(&label, &resp, &[400])?;
                expect_no_session(&label, &resp)?;
                let resp = cx.send_jar(&mut b_jar, Req::get(&b_host, "/")).await?;
                gw_check!(
                    !is_echo(&resp) && b_echo.hits() == 0,
                    "{label}: app B let the visitor in afterwards"
                );

                // (b) The same callback twice, with the cookies the browser had before it.
                let mut jar = Jar::default();
                let start = cx.auth_start(&mut jar, &s.host, "/").await?;
                let cb = cx.authorize(&s.user, &s.host, &start).await?;
                let cookie = jar.header_for(&s.host, "/.ruum/v1/auth/callback");
                let with_cookie = |req: Req| match &cookie {
                    Some(c) => req.header("Cookie", c),
                    None => req,
                };
                let label = format!("GET {} with a sign-in callback", s.host);
                let first = cx.send(with_cookie(Req::get(&s.host, &cb))).await?;
                gw_check!(
                    first.is_redirect()
                        && first
                            .set_cookies()
                            .iter()
                            .any(|c| c.starts_with("__Host-ruum_app=")),
                    "{label}: the first use of the code did not sign in (got {})",
                    first.status
                );
                let label = format!("GET {} with the same callback a second time", s.host);
                let second = cx.send(with_cookie(Req::get(&s.host, &cb))).await?;
                expect_refusal(&label, &second, &[400])?;
                expect_no_session(&label, &second)?;

                // (c) A state cookie, if the gateway sets one, is needed.
                let mut jar = Jar::default();
                let start = cx.auth_start(&mut jar, &s.host, "/").await?;
                let state_cookie = !start.set_cookies().is_empty();
                let cb = cx.authorize(&s.user, &s.host, &start).await?;
                if state_cookie {
                    let label = format!(
                        "GET {} with a sign-in callback without its state cookie",
                        s.host
                    );
                    let resp = cx.send(Req::get(&s.host, &cb)).await?;
                    expect_refusal(&label, &resp, &[400])?;
                    expect_no_session(&label, &resp)?;
                }
                Ok(())
            }
        ),
    ]
}
