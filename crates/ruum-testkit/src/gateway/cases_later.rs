//! The gateway cases of later cards (each in [`super::PENDING`] with its card): the proxy to a
//! sandbox, WebSockets and cold start (G-1b), the browser script and telemetry (G-2), the report
//! origin (G-4), receipts (X-2), partner keys (B-3) and custom domains (W-5).
//!
//! Where the world has the steps already, the body is the real check. Where a later card must
//! add a world step first, the body fails with [`plan`], which names the steps: the card that
//! removes the `PENDING` line writes the body (it can never pass by accident).

use super::{
    Access, Ctx, Echo, Env, Fail, GatewayCase, Relation, Req, SIGN_IN_OR_REFUSED, echoed,
    echoed_header, expect_refusal, files, is_echo, page, scene,
};
use crate::gw_check;
use serde_json::json;

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

/// The body of a case whose world steps a later card adds: it fails and names the steps.
fn plan(card: &str, steps: &str) -> super::CaseResult {
    Err(Fail::Failed(format!(
        "planned case body: {card} writes it when it removes the PENDING line. Steps: {steps}"
    )))
}

pub fn cases() -> Vec<GatewayCase> {
    vec![
        // ---- G-1b -------------------------------------------------------------------------
        case!(
            "proxy_reaches_a_running_sandbox",
            "A node22 deployment is reached through the proxy after the check, with the same \
             cleaning both ways as the test upstream",
            "STEP3 area 4 \"Gateway\" (G-1b); gateway.openapi.yaml \"The check of every request\" \
             3 and 4",
            [],
            |_cx| {
                plan(
                    "G-1b",
                    "seed a node22 deployment whose app echoes its request (a world step \
                     seed_node_deployment), set it live, repeat request_headers_are_cleaned and \
                     app_set_cookie_for_ruum_names_is_dropped against the sandbox",
                )
            }
        ),
        case!(
            "websockets_pass_after_the_check",
            "A WebSocket upgrade passes only after the use check; without a session it gets the \
             sign-in redirect or 401/403 and never reaches the app",
            "STEP3 area 4 \"Gateway\" (G-1b)",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::get(&s.host, "/ws")
                    .header("Connection", "Upgrade")
                    .header("Upgrade", "websocket")
                    .header("Sec-WebSocket-Version", "13")
                    .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
                let label = format!("{} without a session", req.label());
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                gw_check!(s.echo().hits() == 0, "{label}: the app got the upgrade");
                plan(
                    "G-1b",
                    "with the session: expect 101 from a WebSocket echo upstream",
                )
            }
        ),
        case!(
            "a_cold_start_is_held_and_answered",
            "A request to a stopped app is held while it starts and answered (decision 25: 3 s), \
             never dropped",
            "STEP3 area 4 \"Gateway\" (G-1b); decision 0006 G point 3",
            [],
            |_cx| {
                plan(
                    "G-1b",
                    "stop the app's instance (world step), send a request, expect 200",
                )
            }
        ),
        // ---- G-2 --------------------------------------------------------------------------
        case!(
            "the_browser_script_is_served",
            "GET /.ruum/v1/script.js answers the script (text/javascript) from the gateway, \
             never the app",
            "gateway.openapi.yaml /.ruum/v1/script.js",
            ["getBrowserScript"],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::get(&s.host, "/.ruum/v1/script.js");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                gw_check!(
                    resp.status == 200 && !is_echo(&resp),
                    "{label}: expected the gateway's script, got {}",
                    resp.status
                );
                let ct = resp.header("content-type").unwrap_or("");
                gw_check!(ct.contains("javascript"), "{label}: Content-Type {ct:?}");
                Ok(())
            }
        ),
        case!(
            "telemetry_of_a_platform_app_needs_a_session",
            "An app with access platform refuses a telemetry batch without an app session (401)",
            "gateway.openapi.yaml /.ruum/v1/telemetry",
            ["sendBrowserTelemetry"],
            |cx| {
                let s = scene(&cx, true).await?;
                let req = Req::post(&s.host, "/.ruum/v1/telemetry")
                    .json(&json!({"events": [{"kind": "error", "at": "2026-11-13T10:00:00Z"}]}));
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[401])?;
                Ok(())
            }
        ),
        case!(
            "telemetry_ignores_identity_in_the_body",
            "Visit, app and tenant values in a telemetry body are ignored: the gateway takes \
             them from the session and the visit cookie",
            "gateway.openapi.yaml /.ruum/v1/telemetry and TelemetryBatch; decision 0006-13",
            ["sendBrowserTelemetry"],
            |_cx| {
                plan(
                    "G-2",
                    "post a batch with a forged visit, app and tenant in fields and resource \
                     attributes; read the stored events through a world step; expect the \
                     session's app, tenant and visit",
                )
            }
        ),
        // ---- G-4: the report origin --------------------------------------------------------
        case!(
            "the_handoff_needs_a_top_level_navigation",
            "/.ruum/v1/reports/handoff refuses a request whose Sec-Fetch-Mode is not navigate \
             (an app script cannot collect a code)",
            "gateway.openapi.yaml /.ruum/v1/reports/handoff; decision 0006-15",
            ["handOffAppReport"],
            |cx| {
                let s = scene(&cx, true).await?;
                let state = "t6statet6statet6statet6statet6st";
                for mode in ["cors", "no-cors", "same-origin"] {
                    let req =
                        Req::get(&s.host, &format!("/.ruum/v1/reports/handoff?state={state}"))
                            .header("Sec-Fetch-Mode", mode);
                    let label = format!("{} with Sec-Fetch-Mode {mode}", req.label());
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    expect_refusal(&label, &resp, &[400])?;
                }
                Ok(())
            }
        ),
        case!(
            "an_app_script_cannot_read_the_report_preview",
            "The report preview answers only with the report session cookie and the CSRF \
             header: from the app origin (app session, no report session) the gateway refuses \
             it (400, 401, 403 or 404, never the app's answer), and on the platform origin \
             without a report session (401) or with a forged one (401 or 403)",
            "gateway.openapi.yaml /.ruum-report/v1/preview; decision 0006-15; STEP3 card T-6 \
             \"Done when\" (the report origin)",
            ["previewAppReport"],
            |cx| {
                let s = scene(&cx, true).await?;
                // The app origin does not serve the report paths at all.
                let req = Req::get(&s.host, "/.ruum-report/v1/preview")
                    .header("Ruum-Report-Csrf", "t6guess");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, &[400, 401, 403, 404])?;
                // The platform origin without a report session, and with a forged one.
                let p = cx.gw.platform_host.clone();
                let req =
                    Req::get(&p, "/.ruum-report/v1/preview").header("Ruum-Report-Csrf", "t6guess");
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[401])?;
                let req = Req::get(&p, "/.ruum-report/v1/preview")
                    .header("Cookie", "__Host-ruum_report=t6forged")
                    .header("Ruum-Report-Csrf", "t6guess");
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[401, 403])?;
                Ok(())
            }
        ),
        case!(
            "report_choices_are_refused_from_the_app_origin",
            "A report with include choices posted from the app origin (with the app session, no \
             report session and CSRF token) is refused by the gateway (on the app host 400, 401, \
             403 or 404; on the platform host 401 or 403; never the app's answer), and no report \
             is made; choices are accepted only on the report origin",
            "gateway.openapi.yaml /.ruum-report/v1/reports; decision 0006-15",
            ["createAppReport"],
            |cx| {
                let s = scene(&cx, true).await?;
                let body = "--t6b\r\nContent-Disposition: form-data; name=\"description\"\r\n\r\n\
                            broken\r\n--t6b\r\nContent-Disposition: form-data; \
                            name=\"choices\"\r\n\r\n{\"*\":\"include\"}\r\n--t6b--\r\n";
                let refusals: [(String, &[u16]); 2] = [
                    (s.host.clone(), &[400, 401, 403, 404]),
                    (cx.gw.platform_host.clone(), &[401, 403]),
                ];
                for (host, allowed) in refusals {
                    let req = Req::post(&host, "/.ruum-report/v1/reports")
                        .header("Content-Type", "multipart/form-data; boundary=t6b")
                        .header("Origin", &format!("https://{}", s.host))
                        .body(body);
                    let label = req.label();
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    expect_refusal(&label, &resp, allowed)?;
                }
                // A refusal status alone does not prove that nothing was filed.
                plan(
                    "G-4",
                    "after the refusals, read the reports (issues) of the app through a world \
                     step and expect none; a status check alone never passes this case",
                )
            }
        ),
        case!(
            "a_report_names_only_its_own_visit",
            "A report attaches only the records of the reporter's own visit (the gateway's \
             visit-to-trace map), never a visit or trace that the client names",
            "gateway.openapi.yaml /.ruum-report/v1/reports and \"Visits\"; decision 0006-15",
            ["createReportSession", "createAppReport"],
            |_cx| {
                plan(
                    "G-4",
                    "two visitors make traffic; visitor 1 gets a report session (handoff, \
                     createReportSession) and files a report whose context and trace_id name \
                     visitor 2's visit; the issue (world step) holds only visitor 1's records",
                )
            }
        ),
        case!(
            "a_handoff_code_works_once_within_60_seconds",
            "createReportSession redeems a handoff code once, within 60 s, only with the \
             matching state cookie",
            "gateway.openapi.yaml /.ruum-report/v1/session",
            ["createReportSession", "handOffAppReport"],
            |cx| {
                let p = cx.gw.platform_host.clone();
                let req = Req::post(&p, "/.ruum-report/v1/session")
                    .json(&json!({"code": "t6made-up", "state": "t6made-up"}));
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[400])?;
                plan(
                    "G-4",
                    "a real handoff (navigate with state), redeem the code with the state \
                     cookie (201), redeem it again (400), a second code after 60 s (400), a \
                     code with another state (400)",
                )
            }
        ),
        // ---- X-2: receipts -----------------------------------------------------------------
        case!(
            "a_receipt_token_is_shown_once",
            "A reporter who is not a member gets a receipt token once (at least 128 random \
             bits); it resolves only its own report",
            "gateway.openapi.yaml AppReportReceipt; STEP3 \"System lookups\" (receipt token to \
             report); STEP3 card T-6 \"Done when\" (receipts)",
            ["createAppReport"],
            |_cx| {
                plan(
                    "X-2",
                    "file a report on a public app as an anonymous visitor; the receipt token \
                     is in the answer once, has at least 22 base64url characters, resolves its \
                     own report (resolveIssueReceipt) and not another one",
                )
            }
        ),
        // ---- B-3: partner keys -------------------------------------------------------------
        case!(
            "a_partner_key_reaches_only_its_api",
            "A partner key reaches only the external API it was made for (actor partner); on \
             another path or another app it is refused",
            "gateway.openapi.yaml securitySchemes partnerKey; decision 0006-10",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let key = cx.world.partner_key(&s.app, "orders").await?;
                cx.secret(key.expose());
                plan(
                    "B-3",
                    "call the orders API path with the key (actor partner, scopes of the key), \
                     another path and another app's host with it (403)",
                )
            }
        ),
        case!(
            "a_partner_key_never_reaches_the_app",
            "Authorization with a partner key, which the gateway authenticated, is removed \
             before the request is forwarded",
            "decision 0006 G point 2; contracts/apps/README.md \"Rules\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let key = cx.world.partner_key(&s.app, "orders").await?;
                cx.secret(key.expose());
                let req = Req::get(&s.host, "/api/orders")
                    .header("Authorization", &format!("Bearer {}", key.expose()));
                let label = req.label();
                let resp = cx.send(req).await?;
                let v = echoed(&label, &resp)?;
                gw_check!(
                    echoed_header(&v, "authorization").is_empty(),
                    "{label}: the partner key reached the app"
                );
                Ok(())
            }
        ),
        // ---- W-5: custom domains -----------------------------------------------------------
        case!(
            "a_custom_domain_serves_only_when_verified",
            "A custom domain serves its app only when verified (active); a pending domain and a \
             domain of another tenant answer the gateway's 404",
            "decision 0006-16; STEP3 \"Host names and slugs\" and \"System lookups\"",
            [],
            |cx| {
                let w = &cx.world;
                let t = w.seed_tenant("acme").await?;
                let a = w.seed_app(&t, "garden", Access::Platform).await?;
                let d = w
                    .seed_deployment(&a, Env::Live, &files(&[("index.html", &page("g"))]), None)
                    .await?;
                w.set_live(&a, &d).await?;
                let e = Echo::start().await;
                w.set_upstream(&d, &e.url).await?;
                let u = w.seed_user(&t, "mari").await?;
                w.grant(&u, &a, &Relation::View).await?;
                let req = Req::get("garden.t6-unverified.example", "/");
                let label = req.label();
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, &[404])?;
                plan(
                    "W-5",
                    "add a custom domain (world step), pending: 404; verified: the sign-in \
                     redirect; another tenant's pending row for the same host: still the \
                     first tenant's",
                )
            }
        ),
    ]
}
