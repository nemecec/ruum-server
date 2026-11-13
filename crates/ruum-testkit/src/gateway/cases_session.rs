//! The app session (cases 11, 12 and 19 of card T-6): bound to the app and the environment,
//! ended by signing out, and cookies that follow browser rules.

use super::{
    Access, Ctx, Echo, Env, GatewayCase, Jar, Relation, Req, SIGN_IN_OR_REFUSED, SetCookie, echoed,
    expect_refusal, files, is_echo, page, poll_until, scene,
};
use crate::gw_check;
use std::time::Duration;

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

/// The bound of "signing out of Ruum ends the app sessions within a minute".
pub const SIGN_OUT_BOUND: Duration = Duration::from_secs(60);

/// A jar that holds only `__Host-ruum_app=<sid>` for `host` (a session moved to another host).
fn moved(host: &str, sid: &str) -> Jar {
    let mut j = Jar::default();
    j.store(
        host,
        &format!("__Host-ruum_app={sid}; Path=/; Secure; HttpOnly; SameSite=Lax"),
    );
    j
}

pub fn cases() -> Vec<GatewayCase> {
    vec![
        case!(
            "a_session_is_bound_to_its_app",
            "A session cookie of app A sent to app B is refused, also when B's host name was \
             A's before (A removed, its host given to B)",
            "gateway.openapi.yaml \"Sign-in on an app origin\"; STEP3 \"How an app runs\" rule \
             3; decision 0006-16; STEP3 card T-6 \"Done when\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let w = &cx.world;
                let sid = cx.session_of(&s.jar, &s.host)?;
                let b = w.seed_app(&s.tenant, "orchard", Access::Platform).await?;
                let b_live = w
                    .seed_deployment(&b, Env::Live, &files(&[("index.html", &page("b"))]), None)
                    .await?;
                w.set_live(&b, &b_live).await?;
                let b_echo = Echo::start().await;
                w.set_upstream(&b_live, &b_echo.url).await?;
                w.grant(&s.user, &b, &Relation::View).await?;
                let b_host = cx.live_host(&b);
                let req = Req::get(&b_host, "/");
                let label = format!("{} with app A's session", req.label());
                let resp = cx.send_jar(&mut moved(&b_host, &sid), req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                gw_check!(b_echo.hits() == 0, "{label}: app B got the request");

                // A goes away; B takes A's host name.
                let b = w.reuse_host(&s.app, &b).await?;
                let host = cx.live_host(&b);
                gw_check!(
                    host == s.host,
                    "reuse_host gave B the host {host}, expected A's {}",
                    s.host
                );
                let req = Req::get(&host, "/");
                let label = format!("{} with the session of the removed app A", req.label());
                let resp = cx.send_jar(&mut moved(&host, &sid), req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                gw_check!(b_echo.hits() == 0, "{label}: app B got the request");
                Ok(())
            }
        ),
        case!(
            "a_session_is_bound_to_its_environment",
            "A preview session does not open the live app, and a live session does not open the \
             preview",
            "gateway.openapi.yaml \"Sign-in on an app origin\" (bound to the environment and the \
             deployment); STEP3 \"How an app runs\" rule 3",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let w = &cx.world;
                w.grant(&s.user, &s.app, &Relation::Edit).await?;
                let prev = w
                    .seed_deployment(
                        &s.app,
                        Env::Preview,
                        &files(&[("index.html", &page("p"))]),
                        None,
                    )
                    .await?;
                let p_echo = Echo::start().await;
                w.set_upstream(&prev, &p_echo.url).await?;
                let phost = cx.preview_host(&s.app, &prev);
                let (pjar, _) = cx.sign_in(&s.user, &phost).await?;
                let psid = cx.session_of(&pjar, &phost)?;
                let lsid = cx.session_of(&s.jar, &s.host)?;
                let live_hits = s.echo().hits();
                let req = Req::get(&s.host, "/");
                let label = format!("{} with the preview's session", req.label());
                let resp = cx.send_jar(&mut moved(&s.host, &psid), req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                let req = Req::get(&phost, "/");
                let label = format!("{} with the live session", req.label());
                let resp = cx.send_jar(&mut moved(&phost, &lsid), req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                gw_check!(
                    p_echo.hits() == 0 && s.echo().hits() == live_hits,
                    "a session of one environment reached the other"
                );
                // A second preview of the same app: its session is bound to the deployment.
                let prev2 = w
                    .seed_deployment(
                        &s.app,
                        Env::Preview,
                        &files(&[("index.html", &page("q"))]),
                        None,
                    )
                    .await?;
                w.set_upstream(&prev2, &p_echo.url).await?;
                let phost2 = cx.preview_host(&s.app, &prev2);
                let req = Req::get(&phost2, "/");
                let label = format!("{} with another preview's session", req.label());
                let resp = cx.send_jar(&mut moved(&phost2, &psid), req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                gw_check!(
                    p_echo.hits() == 0,
                    "{label}: the other preview got the request"
                );
                Ok(())
            }
        ),
        case!(
            "signing_out_of_ruum_ends_app_sessions",
            "After the user signs out of Ruum, the app session is refused within a minute \
             (bounded poll)",
            "gateway.openapi.yaml \"Sign-in on an app origin\"; card G-1a \"Sign-in on the app \
             origin\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                echoed(&label, &resp)?;
                cx.world.sign_out_platform(&s.user).await?;
                let label = format!("GET {}/ after signing out of Ruum", s.host);
                poll_until(&label, SIGN_OUT_BOUND, Duration::from_secs(1), || {
                    let cx = cx.clone();
                    let mut jar = s.jar.clone();
                    let host = s.host.clone();
                    let label = label.clone();
                    async move {
                        let resp = cx.send_jar(&mut jar, Req::get(&host, "/")).await?;
                        if is_echo(&resp) {
                            return Ok(false);
                        }
                        expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                        Ok(true)
                    }
                })
                .await
            }
        ),
        case!(
            "logout_ends_the_app_session",
            "POST /.ruum/v1/auth/logout answers 204 and the session is refused afterwards",
            "gateway.openapi.yaml /.ruum/v1/auth/logout",
            ["appAuthLogout"],
            |cx| {
                let s = scene(&cx, true).await?;
                let mut jar = s.jar.clone();
                let req = Req::post(&s.host, "/.ruum/v1/auth/logout");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                gw_check!(
                    resp.status == 204 && !is_echo(&resp),
                    "{label}: expected the gateway's 204, got {} {}",
                    resp.status,
                    resp.short_body()
                );
                // Even if the browser kept the cookie, the session is gone server side.
                let req = Req::get(&s.host, "/");
                let label = format!("{} with the old session after logout", req.label());
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                gw_check!(
                    s.echo().hits() == 0,
                    "{label}: the app got a request after logout"
                );
                Ok(())
            }
        ),
        case!(
            "gateway_cookies_follow_browser_rules",
            "Every cookie the gateway sets (__Host-ruum_app, __Host-ruum_visit) is __Host-, \
             Secure, HttpOnly, SameSite=Lax, Path=/ and has no Domain; a jar with browser rules \
             sends it back only to the same host",
            "backlog S-41; gateway.openapi.yaml \"Sign-in on an app origin\" and \"Visits\"; \
             STEP3 \"How an app runs\" rule 3",
            ["appAuthCallback"],
            |cx| {
                let s = scene(&cx, true).await?;
                let (_, signin) = cx.sign_in(&s.user, &s.host).await?;
                let mut raw: Vec<String> = signin.set_cookies.clone();
                let mut jar = s.jar.clone();
                let resp = cx.send_jar(&mut jar, Req::get(&s.host, "/")).await?;
                echoed("GET / after sign-in", &resp)?;
                raw.extend(resp.set_cookies().iter().map(|c| c.to_string()));
                let mut saw_session = false;
                for c in &raw {
                    let Some(sc) = SetCookie::parse(c) else {
                        continue;
                    };
                    let lower = sc.name.to_ascii_lowercase();
                    if !lower.contains("ruum") {
                        continue;
                    }
                    let n = &sc.name;
                    gw_check!(
                        sc.name.starts_with("__Host-"),
                        "the gateway's cookie {n} has no __Host- prefix"
                    );
                    gw_check!(sc.has("secure"), "the gateway's cookie {n} is not Secure");
                    gw_check!(
                        sc.has("httponly"),
                        "the gateway's cookie {n} is not HttpOnly"
                    );
                    gw_check!(
                        sc.attr("samesite")
                            .map(|v| v.eq_ignore_ascii_case("lax"))
                            .unwrap_or(false),
                        "the gateway's cookie {n} is not SameSite=Lax"
                    );
                    gw_check!(
                        sc.attr("path") == Some("/"),
                        "the gateway's cookie {n} has no Path=/"
                    );
                    gw_check!(!sc.has("domain"), "the gateway's cookie {n} has a Domain");
                    if sc.name == "__Host-ruum_app" {
                        saw_session = true;
                    }
                }
                gw_check!(saw_session, "the sign-in set no __Host-ruum_app cookie");
                // The jar sends the session only to the same host.
                for other in [
                    cx.live_host(&super::App {
                        id: String::new(),
                        slug: "orchard".into(),
                        tenant: s.tenant.clone(),
                    }),
                    format!("x.{}", s.host),
                    format!("{}.{}", s.tenant.slug, cx.gw.apps_domain),
                ] {
                    gw_check!(
                        jar.value(&other, "__Host-ruum_app").is_none(),
                        "a browser would send the session cookie to {other}"
                    );
                }
                gw_check!(
                    jar.value(&s.host, "__Host-ruum_app").is_some(),
                    "a browser would not send the session back to its own host"
                );
                Ok(())
            }
        ),
    ]
}
