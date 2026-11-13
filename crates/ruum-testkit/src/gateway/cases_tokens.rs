//! The user token and the credentials that never mix (cases 9, 10, 14 and 20 of card T-6).

use super::token::{Expect, decode_unverified, verify_user_token};
use super::{
    AgentAudience, Ctx, Echo, Env, Fail, GatewayCase, Relation, Req, SIGN_IN_OR_REFUSED, echoed,
    echoed_header, expect_refusal, files, page, scene,
};
use crate::{gw_check, gw_fail};
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

/// The one `Ruum-User` token in an echoed request.
fn user_token_of(label: &str, v: &Value) -> Result<String, Fail> {
    let t = echoed_header(v, "ruum-user");
    if t.len() != 1 {
        gw_fail!(
            "{label}: the app got {} Ruum-User headers, expected one",
            t.len()
        );
    }
    Ok(t[0].clone())
}

/// Verifies `token` with the gateway's key set; the case fails with the reason (no value).
fn verify(cx: &Ctx, label: &str, token: &str, expect: &Expect) -> Result<Value, Fail> {
    verify_user_token(token, &cx.gw.user_token_jwks, expect)
        .map_err(|e| Fail::Failed(format!("{label}: the user token does not verify: {e}")))
}

pub fn cases() -> Vec<GatewayCase> {
    vec![
        case!(
            "the_user_token_follows_the_contract",
            "Ruum-User is on every forwarded request and verifies with the gateway's key set \
             (EdDSA): numeric iat and exp, exp - iat at most 60 s, typ app_user, iss the \
             instance origin, aud app:<app-id> and env live (a preview: \
             app:<app-id>/preview/<deployment-id> and env preview), sub pairwise and the same \
             on every request, no name or email when the app asks for no claims, the tenant \
             slug, the claims of user-token.schema.json (unknown claims allowed)",
            "contracts/apps/user-token.schema.json; contracts/apps/README.md \"Rules\"; decision \
             0006 G point 2; STEP3 \"How an app runs\" rules 3 and 4; design 9.4 and 9.6 \
             (pairwise IDs, only the facts the app may know)",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let live = Expect::live(&cx.gw.issuer, &s.app.id);
                let mut subs = Vec::new();
                for path in ["/", "/a", "/b?c=d"] {
                    let req = Req::get(&s.host, path);
                    let label = req.label();
                    let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                    let v = echoed(&label, &resp)?;
                    let t = user_token_of(&label, &v)?;
                    cx.secret(&t);
                    let c = verify(&cx, &label, &t, &live)?;
                    gw_check!(
                        c["sub"]
                            .as_str()
                            .map(|x| x.starts_with("pairwise:"))
                            .unwrap_or(false),
                        "{label}: sub of a signed-in person must be pairwise:<id>"
                    );
                    gw_check!(
                        c["sub"].as_str() != Some(s.user.id.as_str())
                            && !c["sub"].as_str().unwrap_or("").contains(&s.user.id),
                        "{label}: sub holds the platform user ID"
                    );
                    gw_check!(
                        c["tenant"] == s.tenant.slug.as_str(),
                        "{label}: tenant claim"
                    );
                    gw_check!(
                        c["actor"] == "user",
                        "{label}: actor of a person must be user"
                    );
                    for claim in ["name", "email"] {
                        gw_check!(
                            c.get(claim).is_none(),
                            "{label}: the token has {claim}, but the app asks for no claims"
                        );
                    }
                    subs.push(c["sub"].as_str().unwrap_or("").to_string());
                }
                gw_check!(
                    subs.windows(2).all(|w| w[0] == w[1]),
                    "the sub of the same person changed between requests to the same app"
                );
                // A preview.
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
                let req = Req::get(&host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                let v = echoed(&label, &resp)?;
                let t = user_token_of(&label, &v)?;
                cx.secret(&t);
                verify(
                    &cx,
                    &label,
                    &t,
                    &Expect::preview(&cx.gw.issuer, &s.app.id, &prev.id),
                )?;
                Ok(())
            }
        ),
        case!(
            "a_token_is_per_app_and_per_environment",
            "A token taken from app A's upstream and sent as Ruum-User to app B never reaches B \
             as that token (B gets its own); the reference verifier refuses A's token for B and \
             a preview's token for the live app",
            "contracts/apps/README.md \"Rules\" (previews have their own audience); decision \
             0006-10; STEP3 \"How an app runs\" rule 4; card G-1a \"Done when\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let w = &cx.world;
                let b = w
                    .seed_app(&s.tenant, "orchard", super::Access::Platform)
                    .await?;
                let b_live = w
                    .seed_deployment(&b, Env::Live, &files(&[("index.html", &page("b"))]), None)
                    .await?;
                w.set_live(&b, &b_live).await?;
                let b_echo = Echo::start().await;
                w.set_upstream(&b_live, &b_echo.url).await?;
                w.grant(&s.user, &b, &Relation::View).await?;
                let b_host = cx.live_host(&b);
                let (mut b_jar, _) = cx.sign_in(&s.user, &b_host).await?;

                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                let a_token = user_token_of(&label, &echoed(&label, &resp)?)?;
                cx.secret(&a_token);

                let a_claims = verify(
                    &cx,
                    &label,
                    &a_token,
                    &Expect::live(&cx.gw.issuer, &s.app.id),
                )?;

                let req = Req::get(&b_host, "/").header("Ruum-User", &a_token);
                let label = req.label();
                let resp = cx.send_jar(&mut b_jar, req).await?;
                let b_token = user_token_of(&label, &echoed(&label, &resp)?)?;
                cx.secret(&b_token);
                gw_check!(b_token != a_token, "{label}: app B got app A's token");
                let b_claims = verify(&cx, &label, &b_token, &Expect::live(&cx.gw.issuer, &b.id))?;
                gw_check!(
                    a_claims["sub"] != b_claims["sub"],
                    "{label}: the same person has the same sub at app A and app B (design 9.6: \
                     pairwise, different for every app)"
                );
                gw_check!(
                    verify_user_token(
                        &a_token,
                        &cx.gw.user_token_jwks,
                        &Expect::live(&cx.gw.issuer, &b.id)
                    )
                    .is_err(),
                    "the reference verifier accepted app A's token for app B"
                );
                // A preview's token at the live app.
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
                let phost = cx.preview_host(&s.app, &prev);
                let (mut pjar, _) = cx.sign_in(&s.user, &phost).await?;
                let req = Req::get(&phost, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut pjar, req).await?;
                let p_token = user_token_of(&label, &echoed(&label, &resp)?)?;
                cx.secret(&p_token);
                gw_check!(
                    verify_user_token(
                        &p_token,
                        &cx.gw.user_token_jwks,
                        &Expect::live(&cx.gw.issuer, &s.app.id)
                    )
                    .is_err(),
                    "the reference verifier accepted a preview's token for the live app"
                );
                let req = Req::get(&s.host, "/").header("Ruum-User", &p_token);
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                let live_token = user_token_of(&label, &echoed(&label, &resp)?)?;
                gw_check!(
                    live_token != p_token,
                    "{label}: the live app got the preview's token"
                );
                Ok(())
            }
        ),
        case!(
            "credentials_never_mix",
            "An app session cookie used as a bearer, a user token used as an agent token at the \
             gateway or as an API token at the REST API, an agent token at the REST API and an \
             agent token used as a session cookie: each refused; on the preview the user has \
             edit, so only the kind of credential can be the reason",
            "decision 0006-10; contracts/apps/README.md \"Rules\" (app tokens never work against \
             the REST API); STEP3 card T-6 \"Done when\"",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let sid = cx.session_of(&s.jar, &s.host)?;
                let req = Req::get(&s.host, "/").header("Authorization", &format!("Bearer {sid}"));
                let label = format!("{} with the session ID as a bearer", req.label());
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;

                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut s.jar.clone(), req).await?;
                let user_token = user_token_of(&label, &echoed(&label, &resp)?)?;
                cx.secret(&user_token);
                let hits = s.echo().hits();

                let w = &cx.world;
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
                // With edit the user may open the preview: a refusal there can only come from
                // the kind of credential.
                w.grant(&s.user, &s.app, &Relation::Edit).await?;
                let req = Req::get(&phost, "/").header("Authorization", &format!("Bearer {sid}"));
                let label = format!("{} with the live session ID as a bearer", req.label());
                let resp = cx.send(req).await?;
                expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                for host in [&phost, &s.host] {
                    let req = Req::get(host, "/")
                        .header("Authorization", &format!("Bearer {user_token}"));
                    let label = format!("{} with a user token as an agent token", req.label());
                    let resp = cx.send(req).await?;
                    expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                }
                gw_check!(
                    p_echo.hits() == 0 && s.echo().hits() == hits,
                    "a user token used as a bearer reached an app"
                );

                let st = w.rest_api_status(&user_token).await?;
                gw_check!(
                    st == 401,
                    "the REST API answered {st} to a user token, expected 401"
                );
                let agent = cx.agent_token(&s.user, AgentAudience::Ruumapps).await?;
                let st = w.rest_api_status(agent.expose()).await?;
                gw_check!(
                    st == 401,
                    "the REST API answered {st} to an agent token of the audience ruumapps, \
                     expected 401"
                );

                for host in [&phost, &s.host] {
                    let req = Req::get(host, "/")
                        .header("Cookie", &format!("__Host-ruum_app={}", agent.expose()));
                    let label = format!("{} with an agent token as the session", req.label());
                    let resp = cx.send(req).await?;
                    expect_refusal(&label, &resp, SIGN_IN_OR_REFUSED)?;
                }
                gw_check!(
                    p_echo.hits() == 0 && s.echo().hits() == hits,
                    "an agent token used as a session reached an app"
                );
                Ok(())
            }
        ),
        case!(
            "the_roles_claim_lists_active_app_roles",
            "The roles claim lists the visitor's active app roles",
            "contracts/apps/user-token.schema.json roles; design 9.3; plan seam 3 (B-2's \
             effective_roles)",
            [],
            |cx| {
                let s = scene(&cx, true).await?;
                let member = cx.world.seed_user(&s.tenant, "kati").await?;
                cx.world
                    .grant(&member, &s.app, &Relation::Role("approver".into()))
                    .await?;
                let (mut jar, _) = cx.sign_in(&member, &s.host).await?;
                let req = Req::get(&s.host, "/");
                let label = req.label();
                let resp = cx.send_jar(&mut jar, req).await?;
                let t = user_token_of(&label, &echoed(&label, &resp)?)?;
                cx.secret(&t);
                let (_, c) = decode_unverified(&t).map_err(Fail::Failed)?;
                let roles: Vec<&str> = c["roles"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                gw_check!(
                    roles.contains(&"approver"),
                    "{label}: roles is {roles:?}, expected it to hold approver"
                );
                Ok(())
            }
        ),
    ]
}
