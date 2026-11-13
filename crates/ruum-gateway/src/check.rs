//! The access check of every request (`gateway.openapi.yaml`, "The check of every request" 2;
//! `docs/STEP3.md` "How an app runs" rule 3; decision 0006-10).
//!
//! - A bearer JWT in `Authorization` goes to the core first. An agent token of the audience
//!   `ruumapps` opens a preview when its user has `open_preview` and its scope allows `edit` on
//!   the app; a live host refuses it (`403`). Any other platform credential (an API token, a
//!   share-link token, another audience) is refused (`401`) and never reaches the app. A token
//!   that is not the platform's is the app's own scheme and passes unchanged.
//! - Otherwise the app session (`__Host-ruum_app`): the core says whether it is live, bound to
//!   this app, environment and preview, and whether the person has `use` (live) or
//!   `open_preview` (preview). Without a session the visitor is sent to the sign-in; with a
//!   session but without the right, the `403` page; the app sees nothing in both cases.
//! - `access: public` (live only) passes everybody, but only while the tenant's public approval
//!   is current (backlog S-33); without it the app is treated as `access: platform`.
//! - Partner keys (`ruum_appkey_...`) are refused with `403` until card B-3.
//!
//! Every answer of the core is cached at most a few seconds ([`crate::core_api::CHECK_TTL`]).

use crate::Gateway;
use crate::Site;
use crate::proxy::gateway_cookie;
use crate::session::APP_COOKIE;
use axum::http::HeaderMap;
use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::identity::app_sign_in::wire::{AgentAnswer, SessionInfo};

/// Who passed the check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Visitor {
    /// A visitor of a public app without a session.
    Anonymous,
    /// A signed-in person (the app session).
    User {
        info: SessionInfo,
        /// The session secret (for the visit map; never forwarded).
        session: String,
    },
    /// An agent with an agent token (previews only).
    Agent { sub: String, on_behalf_of: String },
}

/// The result of the check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Forward (or serve) the request. `strip_authorization`: the gateway authenticated the
    /// request with its `Authorization` header.
    Pass {
        visitor: Visitor,
        strip_authorization: bool,
    },
    /// No credentials: go to the sign-in (`GET`/`HEAD`) or `401`.
    SignIn,
    /// A credential of the platform that may not open apps.
    Unauthorized,
    /// Known, but without the right.
    Forbidden,
    /// The core could not answer: fail closed.
    Unavailable,
}

/// The visible prefix of a partner API key (`createAppApiKey`, card B-3).
pub const PARTNER_KEY_PREFIX: &str = "ruum_appkey_";

/// The token of `Authorization: Bearer <token>` when it has the form of a JWT (three base64url
/// parts), or a partner key; `None` for any other value (the app's own scheme).
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .trim();
    let (scheme, token) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    let jwt = token.split('.').count() == 3
        && token.split('.').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    (jwt || token.starts_with(PARTNER_KEY_PREFIX)).then_some(token)
}

/// The check of one request (see the module docs).
pub async fn check(gw: &Gateway, site: &Site, headers: &HeaderMap) -> Outcome {
    if let Some(token) = bearer(headers) {
        if token.starts_with(PARTNER_KEY_PREFIX) {
            // Partner keys reach only their external API: card B-3.
            return Outcome::Forbidden;
        }
        match gw.core.agent(&site.binding, token).await {
            Err(e) => {
                tracing::warn!(target: "ruum::gateway", error = %e, "the agent check failed");
                return Outcome::Unavailable;
            }
            Ok(AgentAnswer::Agent {
                allowed,
                sub,
                on_behalf_of,
            }) => {
                if site.info.environment != AppEnv::Preview || !allowed {
                    return Outcome::Forbidden;
                }
                return Outcome::Pass {
                    visitor: Visitor::Agent { sub, on_behalf_of },
                    strip_authorization: true,
                };
            }
            Ok(AgentAnswer::Platform) => return Outcome::Unauthorized,
            Ok(AgentAnswer::Foreign) => {}
        }
    }
    let public_open = site.info.environment == AppEnv::Live
        && site.info.access == "public"
        && site.info.public_approved;
    let session = match gateway_cookie(headers, APP_COOKIE) {
        Some(secret) => match gw.core.session(&site.binding, &secret).await {
            Ok(info) => info.map(|i| (i, secret)),
            Err(e) => {
                tracing::warn!(target: "ruum::gateway", error = %e, "the session check failed");
                return Outcome::Unavailable;
            }
        },
        None => None,
    };
    match session {
        Some((info, session)) => {
            if public_open || info.allowed {
                Outcome::Pass {
                    visitor: Visitor::User { info, session },
                    strip_authorization: false,
                }
            } else {
                Outcome::Forbidden
            }
        }
        None if public_open => Outcome::Pass {
            visitor: Visitor::Anonymous,
            strip_authorization: false,
        },
        None => Outcome::SignIn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn auth(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_str(v).unwrap(),
        );
        h
    }

    #[test]
    fn only_jwts_and_partner_keys_are_the_gateways_bearers() {
        assert_eq!(bearer(&auth("Bearer aaa.bbb.ccc")), Some("aaa.bbb.ccc"));
        assert_eq!(bearer(&auth("bearer eyJ.eyJ.sig-_")), Some("eyJ.eyJ.sig-_"));
        assert!(bearer(&auth("Bearer ruum_appkey_abc")).is_some());
        for other in [
            "Basic YXBwOnQ2b3du",
            "Bearer opaque-session-id",
            "Bearer a.b",
            "Bearer a..c",
            "Bearer a.b.c.d",
            "Token a.b.c",
        ] {
            assert_eq!(bearer(&auth(other)), None, "{other}");
        }
        assert_eq!(bearer(&HeaderMap::new()), None);
    }
}
