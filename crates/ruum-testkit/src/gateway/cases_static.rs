//! Static files and reserved paths (cases 16 and 17 of card T-6).

use super::{
    Access, Ctx, Echo, Env, GatewayCase, Relation, Req, check_security_headers, expect_refusal,
    files, is_echo, page, scene,
};
use crate::gw_check;

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

/// A navigation request of a browser.
fn navigation(host: &str, target: &str) -> Req {
    Req::get(host, target)
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Dest", "document")
}

/// Every byte value, as a binary file.
fn binary() -> Vec<u8> {
    (0..=255u8).cycle().take(1024).collect()
}

/// The request targets of the traversal case: every form must get the gateway's 404 or 400.
/// `ids` are the IDs of the other deployment and tenant, so a target can name them.
pub fn traversal_targets(ids: &[&str]) -> Vec<String> {
    let mut t: Vec<String> = [
        "/sub/../index.html",
        "/sub/../../canary.txt",
        "/../canary.txt",
        "/sub/..%2findex.html",
        "/sub/..%2Findex.html",
        "/%2e%2e/canary.txt",
        "/%2E%2E/canary.txt",
        "/sub/%2e%2e/index.html",
        "/.%2e/canary.txt",
        "/%2e./canary.txt",
        "/..%2f..%2fcanary.txt",
        "/sub%2fpage.html",
        "/sub%2Fpage.html",
        "/sub\\page.html",
        "/sub%5cpage.html",
        "/sub%5Cpage.html",
        "/..\\canary.txt",
        "/sub/%00page.html",
        "/sub/page.html%00",
        "/sub%00/page.html",
        "/sub/page%0a.html",
        "/sub/page%1f.html",
        "/sub/page%7f.html",
        "//index.html",
        "/sub//page.html",
        "/./index.html",
        "/sub/./page.html",
        "/%2e/index.html",
        "/sub/%2e/page.html",
        "/%252e%252e/canary.txt",
        "/sub/..;/index.html",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for id in ids {
        t.push(format!("/../{id}/canary.txt"));
        t.push(format!("/../../{id}/canary.txt"));
        t.push(format!("/../../../{id}/canary.txt"));
        t.push(format!("/%2e%2e/%2e%2e/{id}/canary.txt"));
        t.push(format!("/..%2f..%2f{id}%2fcanary.txt"));
    }
    t
}

pub fn cases() -> Vec<GatewayCase> {
    vec![
        case!(
            "static_files_are_served_byte_for_byte",
            "A static file is served byte for byte with its media type; <dir>/ and / serve \
             index.html; the answer gets the security headers",
            "plan seam 1 (the site index); card G-1a \"Static files\"; gateway.openapi.yaml \
             \"The check of every request\" 4",
            [],
            |cx| {
                let s = scene(&cx, false).await?;
                let w = &cx.world;
                let bin = binary();
                let css = b"body { color: #123456 }".to_vec();
                let dep = w
                    .seed_deployment(
                        &s.app,
                        Env::Live,
                        &files(&[
                            ("index.html", &page("home")),
                            ("css/site.css", &css),
                            ("img/logo.png", &bin),
                            ("docs/index.html", &page("docs")),
                            ("app.js", b"console.log(1)"),
                        ]),
                        None,
                    )
                    .await?;
                w.set_live(&s.app, &dep).await?;
                let checks: Vec<(&str, Vec<u8>, &str)> = vec![
                    ("/", page("home"), "text/html"),
                    ("/index.html", page("home"), "text/html"),
                    ("/css/site.css", css.clone(), "text/css"),
                    ("/img/logo.png", bin.clone(), "image/png"),
                    ("/docs/", page("docs"), "text/html"),
                    ("/app.js", b"console.log(1)".to_vec(), "javascript"),
                ];
                for (path, want, ty) in checks {
                    let req = Req::get(&s.host, path);
                    let label = req.label();
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    gw_check!(
                        resp.status == 200,
                        "{label}: expected 200, got {} {}",
                        resp.status,
                        resp.short_body()
                    );
                    gw_check!(
                        resp.body == want,
                        "{label}: the body is not the file byte for byte"
                    );
                    let ct = resp.header("content-type").unwrap_or("");
                    gw_check!(
                        ct.contains(ty),
                        "{label}: Content-Type {ct:?}, expected {ty}"
                    );
                    check_security_headers(&label, &resp, Env::Live, &cx.gw.ui_origin)?;
                }
                let req = Req::get(&s.host, "/nope.css");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, &[404])?;
                Ok(())
            }
        ),
        case!(
            "spa_fallback_answers_navigations",
            "With spa_fallback, a navigation to an unknown path gets the fallback file; a \
             request that is not a navigation and a deployment without spa_fallback get the \
             gateway's 404",
            "plan seam 1 (exact match, then <path>/index.html, then spa_fallback for \
             navigations); contracts/manifests/app.schema.json static.spa_fallback",
            [],
            |cx| {
                let s = scene(&cx, false).await?;
                let w = &cx.world;
                let dep = w
                    .seed_deployment(
                        &s.app,
                        Env::Live,
                        &files(&[("index.html", &page("spa")), ("main.js", b"1")]),
                        Some("index.html"),
                    )
                    .await?;
                w.set_live(&s.app, &dep).await?;
                let req = navigation(&s.host, "/orders/42");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                gw_check!(
                    resp.status == 200 && resp.body == page("spa"),
                    "{label}: expected the fallback file, got {} {}",
                    resp.status,
                    resp.short_body()
                );
                let req = Req::get(&s.host, "/missing.js")
                    .header("Accept", "*/*")
                    .header("Sec-Fetch-Mode", "no-cors")
                    .header("Sec-Fetch-Dest", "script");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, &[404])?;
                // Without spa_fallback.
                let plain = w
                    .seed_deployment(
                        &s.app,
                        Env::Live,
                        &files(&[("index.html", &page("x"))]),
                        None,
                    )
                    .await?;
                w.set_live(&s.app, &plain).await?;
                let req = navigation(&s.host, "/orders/42");
                let label = format!("{} without spa_fallback", req.label());
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                expect_refusal(&label, &resp, &[404])?;
                Ok(())
            }
        ),
        case!(
            "traversal_never_leaves_the_deployment",
            "Every traversal form (.., %2e%2e, %2f, \\, %5c, a control byte, an empty segment, \
             .) gets the gateway's own 404 or 400 and never a file of another deployment or \
             tenant; the canary files are read back through their own hosts first",
            "decision 0006 G point 4; plan seam 1; card G-1a \"Static files\"",
            [],
            |cx| {
                let w = &cx.world;
                let acme = w.seed_tenant("acme").await?;
                w.set_public_approval(&acme, true).await?;
                let globex = w.seed_tenant("globex").await?;
                w.set_public_approval(&globex, true).await?;
                let garden = w.seed_app(&acme, "garden", Access::Public).await?;
                let shed = w.seed_app(&acme, "shed", Access::Public).await?;
                let vault = w.seed_app(&globex, "vault", Access::Public).await?;
                let g = w
                    .seed_deployment(
                        &garden,
                        Env::Live,
                        &files(&[
                            ("index.html", &page("garden")),
                            ("sub/page.html", &page("page")),
                        ]),
                        None,
                    )
                    .await?;
                w.set_live(&garden, &g).await?;
                let canary_dep = format!("T6-CANARY-DEPLOYMENT-{}", uuid::Uuid::now_v7());
                let canary_ten = format!("T6-CANARY-TENANT-{}", uuid::Uuid::now_v7());
                let sd = w
                    .seed_deployment(
                        &shed,
                        Env::Live,
                        &files(&[("canary.txt", canary_dep.as_bytes())]),
                        None,
                    )
                    .await?;
                w.set_live(&shed, &sd).await?;
                let vd = w
                    .seed_deployment(
                        &vault,
                        Env::Live,
                        &files(&[("canary.txt", canary_ten.as_bytes())]),
                        None,
                    )
                    .await?;
                w.set_live(&vault, &vd).await?;
                // The canaries exist and are served through their own hosts.
                for (app, want) in [(&shed, &canary_dep), (&vault, &canary_ten)] {
                    let req = Req::get(&cx.live_host(app), "/canary.txt");
                    let label = req.label();
                    let resp = cx.send(req).await?;
                    gw_check!(
                        resp.status == 200 && resp.body == want.as_bytes(),
                        "{label}: the canary was not served through its own host ({}): the \
                         case cannot prove anything",
                        resp.status
                    );
                }
                let host = cx.live_host(&garden);
                let req = Req::get(&host, "/sub/page.html");
                let label = req.label();
                let resp = cx.send(req).await?;
                gw_check!(
                    resp.status == 200 && resp.body == page("page"),
                    "{label}: the plain file was not served ({})",
                    resp.status
                );
                let ids = [
                    sd.id.as_str(),
                    vd.id.as_str(),
                    globex.id.as_str(),
                    acme.id.as_str(),
                    shed.id.as_str(),
                    vault.id.as_str(),
                ];
                for target in traversal_targets(&ids) {
                    let req = Req::get(&host, &target);
                    let label = req.label();
                    let resp = cx.send(req).await?;
                    gw_check!(
                        !resp.text().contains("T6-CANARY"),
                        "{label}: the answer holds a file of another deployment or tenant"
                    );
                    expect_refusal(&label, &resp, &[400, 404])?;
                }
                Ok(())
            }
        ),
        case!(
            "a_large_file_streams_whole",
            "A file over 8 MiB is served whole with the right Content-Length",
            "card G-1a \"Static files\" (stream with a size limit, backlog S-38); plan seam 1",
            [],
            |cx| {
                let s = scene(&cx, false).await?;
                let w = &cx.world;
                let big: Vec<u8> = (0..(9 * 1024 * 1024 + 7))
                    .map(|i: usize| (i.wrapping_mul(31) ^ (i >> 11)) as u8)
                    .collect();
                let dep = w
                    .seed_deployment(
                        &s.app,
                        Env::Live,
                        &[
                            ("index.html".to_string(), page("big")),
                            ("data/big.bin".to_string(), big.clone()),
                        ],
                        None,
                    )
                    .await?;
                w.set_live(&s.app, &dep).await?;
                let req = Req::get(&s.host, "/data/big.bin");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                gw_check!(
                    resp.status == 200,
                    "{label}: expected 200, got {}",
                    resp.status
                );
                let cl = resp.header("content-length").unwrap_or("");
                gw_check!(
                    cl == big.len().to_string(),
                    "{label}: Content-Length {cl:?}, expected {}",
                    big.len()
                );
                gw_check!(
                    blake3::hash(&resp.body) == blake3::hash(&big),
                    "{label}: the body ({} bytes) is not the file ({} bytes)",
                    resp.body.len(),
                    big.len()
                );
                Ok(())
            }
        ),
        case!(
            "reserved_paths_belong_to_the_gateway",
            "Every path under /.ruum/ belongs to the gateway: a static file or an upstream \
             route at /.ruum/x is never served, and an unknown path under /.ruum/ gets the \
             gateway's 404; /.RUUM/x and /%2eruum/x never serve the app's file at /.ruum/x",
            "gateway.openapi.yaml \"Paths the gateway keeps for itself\"; STEP3 card T-6 \"Done \
             when\" (the reserved paths)",
            [],
            |cx| {
                let s = scene(&cx, false).await?;
                let w = &cx.world;
                let dep = w
                    .seed_deployment(
                        &s.app,
                        Env::Live,
                        &files(&[
                            ("index.html", &page("home")),
                            (".ruum/x", b"T6-APP-FILE-AT-RESERVED-PATH"),
                            (".ruum/v1/script.js", b"T6-APP-FILE-AT-RESERVED-PATH"),
                        ]),
                        None,
                    )
                    .await?;
                w.set_live(&s.app, &dep).await?;
                let reserved = [
                    "/.ruum/x",
                    "/.ruum/v1/script.js",
                    "/.ruum/",
                    "/.ruum/v1/nothing",
                ];
                // Unknown paths under /.ruum/: the gateway's own 404. (script.js is served by
                // the gateway from G-2 on, so only its content is checked.)
                let unknown = ["/.ruum/x", "/.ruum/", "/.ruum/v1/nothing"];
                // A URL path is case-sensitive, and a gateway may refuse or pass these; they must
                // never serve the app's file at /.ruum/x.
                let lookalikes = ["/.RUUM/x", "/%2eruum/x", "/%2Eruum/x"];
                for path in reserved.iter().chain(lookalikes.iter()) {
                    let req = Req::get(&s.host, path);
                    let label = req.label();
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    gw_check!(
                        !resp.text().contains("T6-APP-FILE-AT-RESERVED-PATH"),
                        "{label}: the app's file at a reserved path was served"
                    );
                    if unknown.contains(path) {
                        expect_refusal(&label, &resp, &[404])?;
                    }
                }
                // An upstream app with a route at /.ruum/x.
                let echo = Echo::start().await;
                let up = w
                    .seed_deployment(
                        &s.app,
                        Env::Live,
                        &files(&[("index.html", &page("u"))]),
                        None,
                    )
                    .await?;
                w.set_upstream(&up, &echo.url).await?;
                w.set_live(&s.app, &up).await?;
                w.grant(&s.user, &s.app, &Relation::View).await?;
                for path in reserved {
                    let req = Req::get(&s.host, path);
                    let label = req.label();
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    gw_check!(!is_echo(&resp), "{label}: the app answered a reserved path");
                    if unknown.contains(&path) {
                        expect_refusal(&label, &resp, &[404])?;
                    }
                }
                gw_check!(
                    echo.seen().iter().all(|r| !r.target.starts_with("/.ruum/")),
                    "a request under /.ruum/ reached the app"
                );
                Ok(())
            }
        ),
    ]
}
