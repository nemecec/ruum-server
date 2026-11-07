//! Sign-in to WebDAV (decision 0001-7).
//!
//! - HTTP Basic with the user's email and an app password (`ruum_app_...`). The app password is
//!   bound to one tenant. The actor is the user (`on_behalf_of` is null).
//! - `Authorization: Bearer` with an agent access token. The token is bound to one tenant; the
//!   actor is the agent, on behalf of its owner, limited by the agent's scope.
//!
//! Other credentials (user and share-link tokens, API keys, session cookies) are refused, so
//! WebDAV never widens what a credential can do.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode, header};
use base64::Engine;
use ruum_core::Services;
use ruum_core::actor::{Context, PrincipalKind};
use ruum_core::error::{ApiError, Error, ErrorCode};
use ruum_core::identity::auth::{authenticate_bearer, context_for};
use ruum_core::identity::unauthenticated;

/// The realm of the HTTP Basic challenge.
pub const REALM: &str = "Ruum";

/// The context of a WebDAV request, or the `401` answer.
pub(crate) async fn context(
    s: &Services,
    headers: &HeaderMap,
) -> Result<Context, Box<Response<Body>>> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Err(challenge(&unauthenticated(
            "sign in to use WebDAV",
            "use your email and an app password (HTTP Basic), or an agent access token",
        )));
    };
    let who = match parse(value) {
        Some(Credentials::Basic { email, password }) => {
            // The WebDAV listener does not know the peer here; the user agent is enough.
            let client = ruum_core::audit::events::ClientInfo {
                ip: None,
                user_agent: headers
                    .get(header::USER_AGENT)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string),
            };
            s.identity
                .authenticate_app_password_from(&email, &password, &client)
                .await
        }
        Some(Credentials::Bearer(token)) => {
            match authenticate_bearer(s, &token, s.tokens.audience()).await {
                Ok(who) if who.actor.actor.kind == PrincipalKind::Agent => Ok(who),
                Ok(_) => Err(unauthenticated(
                    "WebDAV takes only agent access tokens",
                    "sign in with your email and an app password (HTTP Basic)",
                )),
                Err(e) => Err(e),
            }
        }
        None => Err(unauthenticated(
            "the Authorization header is not valid",
            "use HTTP Basic (email and app password) or Bearer (agent access token)",
        )),
    };
    let who = who.map_err(|e| challenge(&e))?;
    // Both credentials are bound to one tenant, so no tenant header is needed.
    context_for(s, &who, None, None).await.map_err(|e| match e {
        Error::Api(ref api) if api.code == ErrorCode::Unauthenticated => challenge(&e),
        other => Box::new(crate::error::response(&other)),
    })
}

enum Credentials {
    Basic { email: String, password: String },
    Bearer(String),
}

fn parse(value: &HeaderValue) -> Option<Credentials> {
    let value = value.to_str().ok()?.trim();
    let (scheme, rest) = value.split_once(' ')?;
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("basic") {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(rest)
            .ok()?;
        let raw = String::from_utf8(raw).ok()?;
        let (email, password) = raw.split_once(':')?;
        Some(Credentials::Basic {
            email: email.to_string(),
            password: password.to_string(),
        })
    } else if scheme.eq_ignore_ascii_case("bearer") && !rest.is_empty() {
        Some(Credentials::Bearer(rest.to_string()))
    } else {
        None
    }
}

/// The `401` answer with the challenges for both sign-in methods.
fn challenge(e: &Error) -> Box<Response<Body>> {
    let api = match e {
        Error::Api(api) if api.code == ErrorCode::Unauthenticated => api.clone(),
        Error::Api(api) => ApiError::new(ErrorCode::Unauthenticated, api.message.clone()),
        _ => return Box::new(crate::error::response(e)),
    };
    let mut res = crate::error::json(StatusCode::UNAUTHORIZED, &api);
    let h = res.headers_mut();
    h.append(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!("Basic realm=\"{REALM}\", charset=\"UTF-8\""))
            .expect("a valid header"),
    );
    h.append(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!("Bearer realm=\"{REALM}\"")).expect("a valid header"),
    );
    Box::new(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_and_bearer() {
        let b = base64::engine::general_purpose::STANDARD.encode("a@b.test:ruum_app_x:y");
        match parse(&HeaderValue::from_str(&format!("Basic {b}")).unwrap()) {
            Some(Credentials::Basic { email, password }) => {
                assert_eq!(email, "a@b.test");
                assert_eq!(password, "ruum_app_x:y");
            }
            _ => panic!("basic"),
        }
        assert!(matches!(
            parse(&HeaderValue::from_static("bearer abc")),
            Some(Credentials::Bearer(t)) if t == "abc"
        ));
        assert!(parse(&HeaderValue::from_static("Digest x")).is_none());
        assert!(parse(&HeaderValue::from_static("Basic !!!")).is_none());
        assert!(parse(&HeaderValue::from_static("Bearer ")).is_none());
    }
}
