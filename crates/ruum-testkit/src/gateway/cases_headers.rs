//! Cleaning both ways (cases 1 to 5 of card T-6): what the gateway removes from a request before
//! it reaches the app, and from the app's answer before it reaches the browser.

use super::echo::cookie_pairs;
use super::{
    Access, AgentAudience, Ctx, Echo, Env, Fail, GatewayCase, Relation, Req,
    check_security_headers, echoed, echoed_header, echoed_headers, expect_refusal, files,
    frame_ancestors, is_echo, page, scene,
};
use crate::{gw_check, gw_fail};

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

/// A sent trace that must never reach the app.
const SENT_TRACE_ID: &str = "0af7651916cd43dd8448eb211c80319c";

pub fn cases() -> Vec<GatewayCase> {
    vec![
        case!(
            "request_headers_are_cleaned",
            "Incoming Ruum-* and ruum_* headers (any case), X-Forwarded-* (prefix), Forwarded, \
             X-Real-IP, tracestate, baggage, hop-by-hop headers and the names listed in \
             Connection never reach the app; the gateway adds its own Ruum-User, \
             Ruum-Request-Id and traceparent after it removed them; the visitor's IP is not \
             passed",
            "decision 0006 G point 2; gateway.openapi.yaml \"The check of every request\" 3; \
             STEP3 \"How an app runs\" rule 3",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let forged_user = "forged-user-token-t6";
                let req = Req::get(&s.host, "/hello?x=1")
                    .header("Ruum-User", forged_user)
                    .header("ruum-tenant", "t6forged-tenant")
                    .header("RUUM-Request-Id", "t6forged-request-id")
                    .header("Ruum_User", "t6forged-underscore")
                    .header("rUuM_Anything", "t6forged-any")
                    .header("X-Forwarded-For", "203.0.113.7")
                    .header("X-Forwarded-Host", "t6forged.example")
                    .header("x-forwarded-proto", "http")
                    .header("X-Forwarded-Anything", "t6forged-fwd")
                    .header("Forwarded", "for=203.0.113.7;host=t6forged.example")
                    .header("X-Real-IP", "203.0.113.7")
                    .header("tracestate", "t6=forged")
                    .header("baggage", "t6forged=1")
                    .header("Keep-Alive", "timeout=5")
                    .header("Proxy-Authorization", "Basic dDZmb3JnZWQ6eA==")
                    .header("TE", "trailers")
                    // The names in Connection go before the gateway adds its own headers: the
                    // gateway's own Ruum-User and traceparent must still arrive.
                    .header("Connection", "close, X-Drop-Me, Ruum-User, traceparent")
                    .header("X-Drop-Me", "t6forged-dropme")
                    .header("X-Keep", "t6-kept");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                let v = echoed(&label, &resp)?;
                let hs = echoed_headers(&v);
                let allowed_ruum = ["ruum-user", "ruum-request-id"];
                for (n, val) in &hs {
                    let l = n.to_ascii_lowercase();
                    if (l.starts_with("ruum-") || l.starts_with("ruum_"))
                        && !allowed_ruum.contains(&l.as_str())
                    {
                        gw_fail!("{label}: the app got the header {n}");
                    }
                    for dropped in [
                        "x-forwarded-",
                        "forwarded",
                        "x-real-ip",
                        "tracestate",
                        "baggage",
                        "keep-alive",
                        "proxy-authorization",
                        "te",
                        "x-drop-me",
                        "trailer",
                        "upgrade",
                    ] {
                        let hit = if dropped.ends_with('-') {
                            l.starts_with(dropped)
                        } else {
                            l == dropped
                        };
                        gw_check!(!hit, "{label}: the app got the header {n}");
                    }
                    gw_check!(
                        !val.contains("t6forged") && !val.contains(forged_user),
                        "{label}: the app got a forged value in {n}"
                    );
                    gw_check!(
                        !val.contains("203.0.113.7"),
                        "{label}: the app got the forged client address in {n}"
                    );
                    if l != "host" {
                        gw_check!(
                            !val.contains("127.0.0.1") && !val.contains("::1"),
                            "{label}: the app got the visitor's IP address in {n} (design 9.6: \
                             apps know visitors only by a pairwise pseudonym)"
                        );
                    }
                }
                for own in ["ruum-user", "ruum-request-id", "traceparent"] {
                    let n = echoed_header(&v, own).len();
                    gw_check!(
                        n == 1,
                        "{label}: the app got {n} {own} headers, expected exactly the gateway's \
                         one"
                    );
                }
                gw_check!(
                    echoed_header(&v, "x-keep") == vec!["t6-kept".to_string()],
                    "{label}: an ordinary header did not pass unchanged"
                );
                Ok(())
            }
        ),
        case!(
            "gateway_cookies_never_reach_the_app",
            "__Host-ruum_app and __Host-ruum_visit are removed from Cookie (several Cookie \
             headers merged; names matched case-insensitively after a __Host- or __Secure- \
             prefix); other cookies pass unchanged",
            "decision 0006 G point 2; gateway.openapi.yaml \"The check of every request\" 3; \
             contracts/apps/README.md \"Rules\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let sid = cx.session_of(&s.jar, &s.host)?;
                let req = Req::get(&s.host, "/cookies")
                    .header("Cookie", &format!("__Host-ruum_app={sid}; theme=dark"))
                    .header(
                        "Cookie",
                        "__Secure-ruum_app=t6forged1; sid=abc; RUUM_VISIT=t6forged2",
                    )
                    .header(
                        "Cookie",
                        "__host-Ruum_Visit=t6forged3; lang=et; ruum_app=t6forged4; \
                         __Secure-RUUM_visit=t6forged5",
                    );
                let label = req.label();
                let resp = cx.send(req).await?;
                let v = echoed(&label, &resp)?;
                let got: Vec<(String, String)> =
                    cookie_pairs(echoed_header(&v, "cookie").iter().map(String::as_str));
                let mut names: Vec<String> = got.iter().map(|(n, _)| n.clone()).collect();
                names.sort();
                gw_check!(
                    names == vec!["lang", "sid", "theme"],
                    "{label}: the app got the cookies {names:?}, expected exactly [lang, sid, \
                     theme]"
                );
                for (n, val) in &got {
                    let want = match n.as_str() {
                        "theme" => "dark",
                        "sid" => "abc",
                        _ => "et",
                    };
                    gw_check!(val == want, "{label}: the cookie {n} changed on the way");
                }
                for (n, val) in echoed_headers(&v) {
                    gw_check!(
                        !val.contains(&sid) && !val.contains("t6forged"),
                        "{label}: a gateway cookie value reached the app in {n}"
                    );
                }
                Ok(())
            }
        ),
        case!(
            "authorization_of_the_app_passes",
            "Authorization that the gateway did not use (the app's own scheme) passes unchanged",
            "decision 0006 G point 2; gateway.openapi.yaml \"The check of every request\" 3",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::get(&s.host, "/api").header("Authorization", "Basic YXBwOnQ2b3du");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                let v = echoed(&label, &resp)?;
                gw_check!(
                    echoed_header(&v, "authorization") == vec!["Basic YXBwOnQ2b3du".to_string()],
                    "{label}: the app's own Authorization did not pass unchanged"
                );
                Ok(())
            }
        ),
        case!(
            "authorization_used_by_the_gateway_is_removed",
            "An agent token in Authorization, which the gateway authenticated, never reaches the \
             app; the app sees actor agent in Ruum-User instead",
            "decision 0006-10; decision 0006 G point 2; gateway.openapi.yaml securitySchemes \
             agentToken",
            [],
            |cx| {
                let w = &cx.world;
                let tenant = w.seed_tenant("acme").await?;
                let user = w.seed_user(&tenant, "mari").await?;
                let app = w
                    .seed_app(&tenant, "garden", super::Access::Platform)
                    .await?;
                let prev = w
                    .seed_deployment(
                        &app,
                        Env::Preview,
                        &files(&[("index.html", &page("p"))]),
                        None,
                    )
                    .await?;
                let echo = super::Echo::start().await;
                w.set_upstream(&prev, &echo.url).await?;
                w.grant(&user, &app, &Relation::Edit).await?;
                let tok = cx.agent_token(&user, AgentAudience::Ruumapps).await?;
                let host = cx.preview_host(&app, &prev);
                let req = Req::get(&host, "/")
                    .header("Authorization", &format!("Bearer {}", tok.expose()));
                let label = req.label();
                let resp = cx.send(req).await?;
                let v = echoed(&label, &resp)?;
                gw_check!(
                    echoed_header(&v, "authorization").is_empty(),
                    "{label}: the agent token's Authorization reached the app"
                );
                for (n, val) in echoed_headers(&v) {
                    gw_check!(
                        !val.contains(tok.expose()),
                        "{label}: the agent token reached the app in {n}"
                    );
                }
                let ru = echoed_header(&v, "ruum-user");
                let claims = ru
                    .first()
                    .map(|t| super::token::decode_unverified(t))
                    .transpose()
                    .map_err(Fail::Failed)?;
                gw_check!(
                    claims.map(|(_, c)| c["actor"] == "agent").unwrap_or(false),
                    "{label}: the app should see actor agent in Ruum-User"
                );
                Ok(())
            }
        ),
        case!(
            "incoming_traceparent_is_replaced",
            "A traceparent from the internet is replaced by a new trace; the old trace ID is not \
             forwarded",
            "decision 0006-15; gateway.openapi.yaml \"The check of every request\" 3; STEP3 \
             \"How an app runs\" rule 3",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let sent = format!("00-{SENT_TRACE_ID}-b7ad6b7169203331-01");
                let req = Req::get(&s.host, "/trace")
                    .header("traceparent", &sent)
                    .header("tracestate", "t6=1");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                let v = echoed(&label, &resp)?;
                let tp = echoed_header(&v, "traceparent");
                gw_check!(
                    tp.len() == 1,
                    "{label}: the app got {} traceparent headers",
                    tp.len()
                );
                let parts: Vec<&str> = tp[0].split('-').collect();
                let well_formed = parts.len() == 4
                    && parts[0] == "00"
                    && parts[1].len() == 32
                    && parts[2].len() == 16
                    && parts[1].chars().all(|c| c.is_ascii_hexdigit())
                    && parts[1] != "00000000000000000000000000000000";
                gw_check!(
                    well_formed,
                    "{label}: the new traceparent is malformed: {:?}",
                    tp[0]
                );
                gw_check!(
                    parts[1] != SENT_TRACE_ID,
                    "{label}: the incoming trace ID was kept"
                );
                for (n, val) in echoed_headers(&v) {
                    gw_check!(
                        !val.contains(SENT_TRACE_ID),
                        "{label}: the incoming trace ID reached the app in {n}"
                    );
                }
                Ok(())
            }
        ),
        case!(
            "app_set_cookie_for_ruum_names_is_dropped",
            "An app's Set-Cookie whose name starts with ruum or __Host-ruum (any case), whose \
             name is empty (=ruum_app=x) or that has no = is dropped; other cookies pass; the \
             answer gets HSTS, nosniff, Referrer-Policy and frame-ancestors 'none' (live)",
            "decision 0006 G point 2; gateway.openapi.yaml \"The check of every request\" 4; \
             card G-1a \"Done when\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                s.echo().reply_with(&[
                    ("Set-Cookie", "ruum_app=t6bad1; Path=/"),
                    ("Set-Cookie", "__Host-ruum_app=t6bad2; Path=/; Secure"),
                    ("Set-Cookie", "RUUM_session=t6bad3"),
                    ("Set-Cookie", "Ruumish=t6bad4"),
                    ("Set-Cookie", "__host-RUUM_visit=t6bad5; Path=/; Secure"),
                    ("Set-Cookie", "=ruum_app=t6bad6"),
                    ("Set-Cookie", "ruum_app_t6bad7"),
                    ("Set-Cookie", "t6bad8"),
                    ("Set-Cookie", "theme=t6ok1; Path=/"),
                    ("Set-Cookie", "cart=t6ok2; Path=/; HttpOnly"),
                ]);
                let req = Req::get(&s.host, "/set");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                echoed(&label, &resp)?;
                let sc = resp.set_cookies();
                for c in &sc {
                    gw_check!(
                        !c.contains("t6bad"),
                        "{label}: the browser got the app's cookie {:?}",
                        c.split(';').next().unwrap_or("")
                    );
                }
                for ok in ["theme=t6ok1", "cart=t6ok2"] {
                    gw_check!(
                        sc.iter().any(|c| c.starts_with(ok)),
                        "{label}: the app's ordinary cookie {ok} did not pass"
                    );
                }
                check_security_headers(&label, &resp, Env::Live, &cx.gw.ui_origin)?;
                Ok(())
            }
        ),
        case!(
            "frame_ancestors_follow_the_environment",
            "A live app keeps its own frame-ancestors; a live app whose CSP has no \
             frame-ancestors still gets frame-ancestors 'none'; a preview allows only the Ruum UI \
             origin, whatever the app sends",
            "gateway.openapi.yaml \"The check of every request\" 4",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                s.echo().reply_with(&[(
                    "Content-Security-Policy",
                    "frame-ancestors https://partner.example",
                )]);
                let req = Req::get(&s.host, "/own-csp");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                echoed(&label, &resp)?;
                let csp = resp.headers_all("content-security-policy").join(", ");
                let fa = frame_ancestors(&csp);
                gw_check!(
                    !fa.is_empty() && fa.iter().all(|d| d == "https://partner.example"),
                    "{label}: the live app's own frame-ancestors was not kept alone: {csp:?}"
                );
                // A CSP of the app without frame-ancestors.
                s.echo()
                    .reply_with(&[("Content-Security-Policy", "default-src 'self'")]);
                let req = Req::get(&s.host, "/csp-without-frame-ancestors");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                echoed(&label, &resp)?;
                check_security_headers(&label, &resp, Env::Live, &cx.gw.ui_origin)?;
                // A preview of the same app with the same upstream.
                let w = &cx.world;
                let prev = w
                    .seed_deployment(
                        &s.app,
                        Env::Preview,
                        &files(&[("index.html", &page("p"))]),
                        None,
                    )
                    .await?;
                w.set_upstream(&prev, &s.echo().url).await?;
                w.grant(&s.user, &s.app, &Relation::Edit).await?;
                let host = cx.preview_host(&s.app, &prev);
                let (mut jar, _) = cx.sign_in(&s.user, &host).await?;
                let req = Req::get(&host, "/preview-csp");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                echoed(&label, &resp)?;
                check_security_headers(&label, &resp, Env::Preview, &cx.gw.ui_origin)?;
                Ok(())
            }
        ),
        case!(
            "hostile_framing_never_passes_the_check",
            "Two Host headers get 400; an absolute-form target that names another app's host \
             never reaches that app; two different Content-Length values get 400; \
             Content-Length with Transfer-Encoding: chunked gets 400 or is read by the chunked \
             framing alone (RFC 9112 6.1), and no smuggled second request ever reaches the app",
            "decision 0006 G point 2 (the gateway decides on the request it checked); RFC 9112 \
             3.2 and 6.1; gateway.openapi.yaml \"The check of every request\" 1 and 3",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let w = &cx.world;
                // App B of the same tenant: the user has no right on it.
                let b = w.seed_app(&s.tenant, "orchard", Access::Platform).await?;
                let b_live = w
                    .seed_deployment(&b, Env::Live, &files(&[("index.html", &page("b"))]), None)
                    .await?;
                w.set_live(&b, &b_live).await?;
                let b_echo = Echo::start().await;
                w.set_upstream(&b_live, &b_echo.url).await?;
                let b_host = cx.live_host(&b);
                let a_hits = s.echo().hits();

                let req = Req::get(&s.host, "/two-hosts").header("Host", &b_host);
                let label = format!("{} with a second Host: {b_host}", req.label());
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, &[400])?;
                gw_check!(
                    s.echo().hits() == a_hits && b_echo.hits() == 0,
                    "{label}: a request with two Host headers reached an app"
                );

                let req = Req::get(&s.host, &format!("http://{b_host}/absolute"));
                let label = format!("{} (absolute form, Host of app A)", req.label());
                cx.send_jar(&mut s.jar.clone(), req).await?;
                gw_check!(
                    b_echo.hits() == 0,
                    "{label}: the request reached app B, where the visitor has no right"
                );

                let smuggled = format!(
                    "GET /t6smuggled HTTP/1.1\r\nHost: {}\r\nRuum-User: t6forged\r\n\r\n",
                    s.host
                );
                let body = format!("abcdef{smuggled}");
                let req = Req::post(&s.host, "/two-lengths")
                    .header("Content-Length", "6")
                    .header("Content-Length", &body.len().to_string())
                    .body(body);
                let label = format!("{} with two different Content-Length values", req.label());
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, &[400])?;

                let body = format!("0\r\n\r\n{smuggled}");
                let req = Req::post(&s.host, "/cl-te")
                    .header("Content-Length", &body.len().to_string())
                    .header("Transfer-Encoding", "chunked")
                    .body(body);
                let label = format!(
                    "{} with Content-Length and Transfer-Encoding: chunked",
                    req.label()
                );
                let before = s.echo().hits();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                if is_echo(&resp) {
                    // Read by the chunked framing alone: one request with an empty body, and
                    // never both framings forwarded.
                    let seen = s.echo().seen();
                    gw_check!(
                        seen.len() == before + 1,
                        "{label}: the app got {} requests, expected one",
                        seen.len() - before
                    );
                    let last = &seen[seen.len() - 1];
                    gw_check!(
                        last.body.is_empty(),
                        "{label}: the app got the bytes after the last chunk as the body"
                    );
                    gw_check!(
                        last.header_all("content-length").is_empty()
                            || last.header_all("transfer-encoding").is_empty(),
                        "{label}: the gateway forwarded both Content-Length and \
                         Transfer-Encoding"
                    );
                } else {
                    expect_refusal(&label, &resp, &[400])?;
                }
                // Give a smuggled request time to arrive, then look.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                gw_check!(
                    s.echo()
                        .seen()
                        .iter()
                        .chain(b_echo.seen().iter())
                        .all(|r| !r.target.contains("t6smuggled")),
                    "a smuggled second request reached the app"
                );
                Ok(())
            }
        ),
    ]
}
