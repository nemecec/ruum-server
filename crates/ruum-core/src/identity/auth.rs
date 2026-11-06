//! Authentication of a request, for every face of the API (REST, MCP, WebDAV).
//!
//! - [`authenticate_bearer`]: a Ruum access token (`typ` `user`, `agent` or `share_link`). For an
//!   agent the agent row is read on each request (scope, disabled), so a scope change or a
//!   disable applies at once. Agent API keys and app passwords are never bearer tokens.
//! - [`authenticate_session`]: the session cookie of the web UI. The CSRF check (double submit)
//!   is the job of the HTTP layer, which sees the headers.
//! - [`context_for`]: the [`Context`] of the request in one tenant: the tenant the credentials
//!   are bound to, or the `Ruum-Tenant` header of a user, who must be a member or guest there.

use super::{Authenticated, TokenType, unauthenticated};
use crate::actor::{Actor, Context, PrincipalKind, PrincipalRef};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{PrincipalId, TenantId};
use crate::services::Services;

/// Checks a bearer token for the API whose audience is `audience`.
///
/// # The audience of each face
///
/// The protected resource metadata (`/.well-known/oauth-protected-resource`) says
/// `resource = <origin>`, so an MCP client that follows RFC 9728 and RFC 8707 asks for
/// `resource=<origin>` and gets tokens with `aud = <origin>`. So:
/// - the REST API passes `TokenIssuer::audience()` (the origin);
/// - the MCP endpoint calls [`authenticate_mcp_bearer`], which takes `aud = <origin>` and also
///   `aud = <origin>/mcp` (a client that sent `resource=<origin>/mcp`). Do not call this
///   function with `<origin>/mcp` alone: tokens of MCP clients would get `401`.
pub async fn authenticate_bearer(
    s: &Services,
    token: &str,
    audience: &str,
) -> Result<Authenticated> {
    authenticate_bearer_for(s, token, &[audience]).await
}

/// Checks a bearer token for the MCP endpoint: the audience is the API origin
/// (`TokenIssuer::audience`, what the protected resource metadata says) or `<origin>/mcp`.
pub async fn authenticate_mcp_bearer(s: &Services, token: &str) -> Result<Authenticated> {
    let origin = s.tokens.audience().trim_end_matches('/').to_string();
    let mcp = format!("{origin}/mcp");
    authenticate_bearer_for(s, token, &[&origin, &mcp]).await
}

async fn authenticate_bearer_for(
    s: &Services,
    token: &str,
    audiences: &[&str],
) -> Result<Authenticated> {
    if token.starts_with(crate::agents::API_KEY_PREFIX) {
        return Err(unauthenticated(
            "an API key is not an access token",
            "exchange the key for an access token at POST /v1/oauth/token (token exchange)",
        ));
    }
    if token.starts_with(super::APP_PASSWORD_PREFIX) {
        return Err(unauthenticated(
            "app passwords work only for WebDAV",
            "use the web session, or an agent access token",
        ));
    }
    let claims = s.tokens.verify(token)?;
    if !audiences
        .iter()
        .any(|a| claims.aud == a.trim_end_matches('/'))
    {
        return Err(unauthenticated(
            "the access token is for another audience",
            "get a token for this API",
        ));
    }
    match claims.typ {
        TokenType::User => {
            let user = s
                .identity
                .get_user(crate::ids::UserId(claims.sub))
                .await
                .map_err(|_| {
                    unauthenticated("the user of the token does not exist", "sign in again")
                })?;
            Ok(Authenticated {
                actor: Actor::user(user.id),
                bound_tenant: claims.tid,
                scope: None,
                session_id: None,
            })
        }
        TokenType::Agent => {
            let (Some(tenant), Some(agent_id)) = (claims.tid, claims.agent) else {
                return Err(unauthenticated(
                    "the agent token has no tenant or agent",
                    "get a new access token",
                ));
            };
            if agent_id.0 != claims.sub {
                return Err(unauthenticated(
                    "the agent token is not valid",
                    "get a new access token",
                ));
            }
            let agent = s.agents.authenticate(tenant, agent_id).await?;
            if claims.obo.is_some_and(|u| u != agent.owner_id) {
                return Err(unauthenticated(
                    "the agent token is not valid",
                    "get a new access token",
                ));
            }
            Ok(Authenticated {
                actor: Actor {
                    actor: PrincipalRef {
                        kind: PrincipalKind::Agent,
                        id: agent.id.into(),
                    },
                    on_behalf_of: Some(agent.owner_id),
                },
                bound_tenant: Some(tenant),
                scope: Some(agent.scope.as_scope()),
                session_id: None,
            })
        }
        TokenType::ShareLink => {
            let (Some(tenant), Some(link), Some(scope)) =
                (claims.tid, claims.share_link, claims.scope)
            else {
                return Err(unauthenticated(
                    "the share link token is not valid",
                    "open the share link again",
                ));
            };
            // A tenant that no longer allows share links stops every link at once.
            let t = s.tenants.find(tenant).await.map_err(|_| {
                unauthenticated(
                    "the share link token is not valid",
                    "open the share link again",
                )
            })?;
            if !t.policy.allow_share_links {
                return Err(unauthenticated(
                    "share links are turned off in this tenant",
                    "ask the owner for access",
                ));
            }
            // The same for edit links: a token of an edit link stops when the tenant turns
            // edit links off, not only at its expiry (the role comes from the signed claims).
            if scope.max_role > crate::authz::Role::Comment && !t.policy.allow_edit_links {
                return Err(unauthenticated(
                    "edit links are turned off in this tenant",
                    "ask the owner for access",
                ));
            }
            Ok(Authenticated {
                actor: Actor {
                    actor: PrincipalRef {
                        kind: PrincipalKind::ShareLink,
                        id: PrincipalId(link.0),
                    },
                    on_behalf_of: None,
                },
                bound_tenant: Some(tenant),
                scope: Some(scope),
                session_id: None,
            })
        }
    }
}

/// Checks the secret of the session cookie.
pub async fn authenticate_session(s: &Services, secret: &str) -> Result<Authenticated> {
    let session = s
        .identity
        .session_by_secret(secret)
        .await
        .map_err(|e| match e {
            Error::Api(_) => {
                unauthenticated("the session has ended", "sign in again at /v1/auth/login")
            }
            other => other,
        })?;
    Ok(Authenticated {
        actor: Actor::user(session.user_id),
        bound_tenant: None,
        scope: None,
        session_id: Some(session.id),
    })
}

/// The context of a request in one tenant.
///
/// - Credentials bound to a tenant (agent and share-link tokens): that tenant. A `tenant`
///   header that names another tenant fails with `permission_denied`.
/// - A user: the `tenant` header is required (`bad_request` without it), and the user must be a
///   member or guest of that tenant; otherwise the tenant is `not_found`.
pub async fn context_for(
    s: &Services,
    who: &Authenticated,
    tenant: Option<TenantId>,
    idempotency_key: Option<String>,
) -> Result<Context> {
    let tenant = match (who.bound_tenant, tenant) {
        (Some(bound), Some(asked)) if bound != asked => {
            return Err(Error::Api(
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    "the credentials are bound to another tenant",
                )
                .with_hint("leave out the Ruum-Tenant header, or send the tenant of the token"),
            ));
        }
        (Some(bound), _) => bound,
        (None, Some(asked)) => asked,
        (None, None) => {
            return Err(Error::Api(
                ApiError::new(ErrorCode::BadRequest, "the Ruum-Tenant header is missing")
                    .with_hint(
                        "send the tenant ID in the Ruum-Tenant header (see GET /v1/tenants)",
                    ),
            ));
        }
    };
    if who.bound_tenant.is_none() {
        let user = who.user().ok_or_else(|| Error::not_found("tenant"))?;
        if s.tenants.role_of(tenant, user).await?.is_none() {
            return Err(Error::not_found("tenant"));
        }
    }
    // The checks of this request read at least as fresh as the tenant's newest relationship
    // write from any process (decision 0002-4). A lookup error fails the request.
    crate::authz::load_request_floor(&*s.freshness, tenant).await?;
    Ok(Context {
        tenant,
        actor: who.actor,
        scope: who.scope.clone(),
        idempotency_key,
    })
}

#[cfg(test)]
mod audience_tests {
    use super::*;
    use crate::identity::{AccessClaims, VerifiedLogin};

    /// MCP clients ask for `resource=<origin>` (the protected resource metadata), so the MCP
    /// face takes `aud = <origin>` as well as `<origin>/mcp`; the REST face only the origin.
    #[tokio::test]
    async fn mcp_takes_the_origin_and_its_mcp_url() {
        let mut s = Services::in_memory();
        // An identity store without a relationship engine (the in-memory engine is not built).
        s.identity = std::sync::Arc::new(crate::identity::memory::MemoryIdentityStore::default());
        let user = s
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://idp.test".into(),
                subject: "aud-1".into(),
                email: Some("aud@example.test".into()),
                email_verified: true,
                name: None,
                id_token: None,
            })
            .await
            .unwrap();
        let origin = s.tokens.audience().to_string();
        let token_for = |aud: &str| {
            let mut c = AccessClaims::new(s.tokens.as_ref(), TokenType::User, user.id.0, 300);
            c.aud = aud.to_string();
            s.tokens.issue(&c).unwrap()
        };
        let plain = token_for(&origin);
        let mcp = token_for(&format!("{origin}/mcp"));
        assert!(authenticate_mcp_bearer(&s, &plain).await.is_ok());
        assert!(authenticate_mcp_bearer(&s, &mcp).await.is_ok());
        assert!(authenticate_bearer(&s, &plain, &origin).await.is_ok());
        assert!(authenticate_bearer(&s, &mcp, &origin).await.is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{Role, Scope};
    use crate::identity::{AccessClaims, VerifiedLogin};
    use crate::ids::ShareLinkId;
    use crate::tenants::TenantPolicyChange;
    use uuid::Uuid;

    fn link_token(s: &Services, tenant: TenantId, role: Role) -> String {
        let now = crate::now().unix_timestamp();
        let link = ShareLinkId::new();
        s.tokens
            .issue(&AccessClaims {
                iss: s.tokens.audience().into(),
                aud: s.tokens.audience().into(),
                typ: TokenType::ShareLink,
                sub: link.0,
                tid: Some(tenant),
                obo: None,
                agent: None,
                share_link: Some(link),
                scope: Some(Scope {
                    subtree: None,
                    max_role: role,
                    expires_at: None,
                }),
                iat: now,
                exp: now + 600,
                jti: Uuid::now_v7(),
            })
            .unwrap()
    }

    /// Tokens of edit links stop when the tenant turns edit links off; view and comment links
    /// go on.
    #[tokio::test]
    async fn edit_link_tokens_stop_when_edit_links_are_turned_off() {
        let s = Services::in_memory();
        let user = s
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://idp.ruum.test".into(),
                subject: "links".into(),
                email: Some("links@example.test".into()),
                email_verified: true,
                name: None,
                id_token: None,
            })
            .await
            .unwrap();
        let tenant = user.home_tenant_id;
        let ctx = Context {
            tenant,
            actor: Actor::user(user.id),
            scope: None,
            idempotency_key: None,
        };
        let on = TenantPolicyChange {
            allow_edit_links: Some(true),
            ..TenantPolicyChange::default()
        };
        s.tenants.update(&ctx, None, Some(on)).await.unwrap();
        let aud = s.tokens.audience().to_string();
        let edit = link_token(&s, tenant, Role::Edit);
        let view = link_token(&s, tenant, Role::View);
        let comment = link_token(&s, tenant, Role::Comment);
        assert!(authenticate_bearer(&s, &edit, &aud).await.is_ok());
        let off = TenantPolicyChange {
            allow_edit_links: Some(false),
            ..TenantPolicyChange::default()
        };
        s.tenants.update(&ctx, None, Some(off)).await.unwrap();
        let err = authenticate_bearer(&s, &edit, &aud).await.unwrap_err();
        assert_eq!(err.to_api().code, ErrorCode::Unauthenticated);
        assert!(authenticate_bearer(&s, &view, &aud).await.is_ok());
        assert!(authenticate_bearer(&s, &comment, &aud).await.is_ok());
    }
}
