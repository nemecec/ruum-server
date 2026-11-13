//! Tenant isolation on app hosts: the same app slug in two tenants, and credentials of one tenant
//! sent to the other (review of card T-6, required change 3; CLAUDE.md "Every query filters by
//! tenant").

use super::token::{Expect, verify_user_token};
use super::{
    Access, AgentAudience, Ctx, Echo, Env, Fail, GatewayCase, Relation, Req, SIGN_IN_OR_REFUSED,
    echoed, echoed_header, expect_refusal, files, jar_with_session, page, scene_with,
};
use crate::gw_check;
use serde_json::Value;

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

/// The one `Ruum-User` token of an echoed request (a secret).
fn token_of(cx: &Ctx, label: &str, v: &Value) -> Result<String, Fail> {
    let t = echoed_header(v, "ruum-user");
    gw_check!(
        t.len() == 1,
        "{label}: the app got {} Ruum-User headers, expected one",
        t.len()
    );
    cx.secret(&t[0]);
    Ok(t[0].clone())
}

pub fn cases() -> Vec<GatewayCase> {
    vec![case!(
        "credentials_never_cross_tenants",
        "With the app slug garden in tenants acme and globex: a session of garden.acme is \
         refused on garden.globex (the user is a member of both and has view on both apps); a \
         globex member who is not an acme member is refused on garden.acme; an agent token of a \
         globex user with edit on globex's preview is refused on acme's preview; a globex user \
         token sent as Ruum-User to acme never reaches acme as that token",
        "CLAUDE.md (tenant isolation); decision 0006-10 and 0006-16; gateway.openapi.yaml \
         \"Sign-in on an app origin\" and \"The check of every request\" 2 and 3; STEP3 \"How an \
         app runs\" rule 3",
        [],
        |cx| {
            // acme: garden, live, an echo upstream, mari with view, signed in.
            let a = scene_with(&cx, "acme", "garden", true).await?;
            let w = &cx.world;
            let globex = w.seed_tenant("globex").await?;
            let g_app = w.seed_app(&globex, "garden", Access::Platform).await?;
            let g_live = w
                .seed_deployment(
                    &g_app,
                    Env::Live,
                    &files(&[("index.html", &page("globex"))]),
                    None,
                )
                .await?;
            w.set_live(&g_app, &g_live).await?;
            let g_echo = Echo::start().await;
            w.set_upstream(&g_live, &g_echo.url).await?;
            let g_host = cx.live_host(&g_app);
            gw_check!(
                g_host != a.host,
                "the two tenants' apps got the same host {g_host}"
            );

            // (a) acme's session on globex's host, for a person with view on both.
            w.seed_member(&a.user, &globex).await?;
            w.grant(&a.user, &g_app, &Relation::View).await?;
            let sid = cx.session_of(&a.jar, &a.host)?;
            let req = Req::get(&g_host, "/");
            let label = format!("{} with the session of garden.acme", req.label());
            let resp = cx
                .send_jar(&mut jar_with_session(&g_host, &sid), req)
                .await?;
            expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
            gw_check!(g_echo.hits() == 0, "{label}: globex's app got the request");
            // The same person with a sign-in of its own on globex passes: the refusal above came
            // from the session's binding, not from a missing right.
            let (mut gjar, _) = cx.sign_in(&a.user, &g_host).await?;
            let req = Req::get(&g_host, "/");
            let label = req.label();
            let resp = cx.send_jar(&mut gjar, req).await?;
            let g_token = token_of(&cx, &label, &echoed(&label, &resp)?)?;

            // (d) globex's user token sent as Ruum-User to acme.
            let a_hits = a.echo().hits();
            let req = Req::get(&a.host, "/").header("Ruum-User", &g_token);
            let label = format!("{} with a user token of globex as Ruum-User", req.label());
            let resp = cx.send_jar(&mut a.jar.clone(), req).await?;
            let a_token = token_of(&cx, &label, &echoed(&label, &resp)?)?;
            gw_check!(a_token != g_token, "{label}: acme's app got globex's token");
            verify_user_token(
                &a_token,
                &cx.gw.user_token_jwks,
                &Expect::live(&cx.gw.issuer, &a.app.id),
            )
            .map_err(|e| Fail::Failed(format!("{label}: acme's token does not verify: {e}")))?;
            gw_check!(
                verify_user_token(
                    &g_token,
                    &cx.gw.user_token_jwks,
                    &Expect::live(&cx.gw.issuer, &a.app.id)
                )
                .is_err(),
                "the reference verifier accepted globex's token for acme's app"
            );
            let a_hits = a_hits + 1;

            // (b) A globex member who is not an acme member, on garden.acme.
            let kalle = w.seed_user(&globex, "kalle").await?;
            w.grant(&kalle, &g_app, &Relation::View).await?;
            if let Some((mut kjar, _)) = cx.try_sign_in(&kalle, &a.host).await? {
                let req = Req::get(&a.host, "/");
                let label = format!("{} as a member of globex only", req.label());
                let resp = cx.send_jar(&mut kjar, req).await?;
                expect_refusal(&label, &resp, &[403])?;
            }
            gw_check!(
                a.echo().hits() == a_hits,
                "a member of globex only reached acme's app"
            );

            // (c) An agent token of a globex user with edit on globex's preview, on acme's
            // preview of the same slug.
            let g_prev = w
                .seed_deployment(
                    &g_app,
                    Env::Preview,
                    &files(&[("index.html", &page("gp"))]),
                    None,
                )
                .await?;
            let gp_echo = Echo::start().await;
            w.set_upstream(&g_prev, &gp_echo.url).await?;
            w.grant(&kalle, &g_app, &Relation::Edit).await?;
            let tok = cx.agent_token(&kalle, AgentAudience::Ruumapps).await?;
            let bearer = format!("Bearer {}", tok.expose());
            let req =
                Req::get(&cx.preview_host(&g_app, &g_prev), "/").header("Authorization", &bearer);
            let label = req.label();
            let resp = cx.send(req).await?;
            echoed(&label, &resp)?;
            let a_prev = w
                .seed_deployment(
                    &a.app,
                    Env::Preview,
                    &files(&[("index.html", &page("ap"))]),
                    None,
                )
                .await?;
            let ap_echo = Echo::start().await;
            w.set_upstream(&a_prev, &ap_echo.url).await?;
            let req =
                Req::get(&cx.preview_host(&a.app, &a_prev), "/").header("Authorization", &bearer);
            let label = format!("{} with an agent token of globex", req.label());
            let resp = cx.send(req).await?;
            expect_refusal(&label, &resp, &[401, 403])?;
            gw_check!(
                ap_echo.hits() == 0,
                "{label}: acme's preview got the request of globex's agent"
            );
            Ok(())
        }
    )]
}
