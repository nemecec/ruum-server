//! Routes of the `auth` area: sign-in through Zitadel (OIDC code flow with PKCE), sessions, the
//! signed-in principal and WebDAV app passwords. See `docs/STEP1.md` for the owner of this file.

use crate::auth::{
    JsonBody, Principal, QueryParams, RequestContext, cookie, refuse_share_link, require_user,
};
use crate::error::{ApiResult, HttpError};
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::{LOCATION, SET_COOKIE, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use ruum_core::identity::{
    CSRF_COOKIE, LOGIN_COOKIE, LoginStart, SESSION_COOKIE, SESSION_DAYS, secret,
};
use ruum_core::{ApiError, AppPasswordId, Error, ErrorCode, PrincipalKind};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

/// How long the sign-in state cookie lives.
const LOGIN_SECONDS: i64 = 600;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/login", get(login_start))
        .route("/v1/auth/callback", get(login_callback))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/me", get(get_me))
        .route("/v1/me/identities", get(list_my_identities))
        .route("/v1/me/sessions", get(list_my_sessions))
        .route("/v1/me/sessions/{session_id}", delete(end_session))
        .route(
            "/v1/me/app-passwords",
            get(list_app_passwords).post(create_app_password),
        )
        .route(
            "/v1/me/app-passwords/{app_password_id}",
            delete(revoke_app_password),
        )
}

/// A `Set-Cookie` value. `Secure` when the public URL is `https`.
fn set_cookie(
    state: &AppState,
    name: &str,
    value: &str,
    path: &str,
    max_age: i64,
    http_only: bool,
) -> String {
    let mut c = format!("{name}={value}; Path={path}; Max-Age={max_age}; SameSite=Lax");
    if http_only {
        c.push_str("; HttpOnly");
    }
    if state.config.public_url.starts_with("https://") {
        c.push_str("; Secure");
    }
    c
}

fn bad_request(message: &str, hint: &str) -> HttpError {
    HttpError(Error::Api(
        ApiError::new(ErrorCode::BadRequest, message).with_hint(hint),
    ))
}

fn redirect(location: &str, cookies: &[String]) -> ApiResult<Response> {
    let mut res = StatusCode::FOUND.into_response();
    res.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(location).map_err(|_| bad_request("bad redirect", "start again"))?,
    );
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            res.headers_mut().append(SET_COOKIE, v);
        }
    }
    Ok(res)
}

/// Whether `return_to` is a relative path of the web UI (`^/[^/\\]`, or exactly `/`).
fn valid_return_to(r: &str) -> bool {
    r == "/"
        || (r.starts_with('/')
            && r.chars().nth(1).is_some_and(|c| c != '/' && c != '\\')
            && !r.chars().any(|c| c.is_control()))
}

#[derive(Deserialize)]
pub struct LoginQuery {
    return_to: Option<String>,
    idp: Option<String>,
}

/// Start OIDC sign-in. `GET /v1/auth/login` (operationId `loginStart`).
pub async fn login_start(
    State(state): State<AppState>,
    QueryParams(q): QueryParams<LoginQuery>,
) -> ApiResult<Response> {
    let return_to = q.return_to.unwrap_or_else(|| "/".into());
    if !valid_return_to(&return_to) {
        return Err(bad_request(
            "return_to must be a relative path of the web UI",
            "use a path such as /files",
        ));
    }
    if let Some(idp) = &q.idp
        && !matches!(idp.as_str(), "google" | "microsoft" | "passkey")
    {
        return Err(bad_request(
            "idp must be google, microsoft or passkey",
            "leave idp out to choose at the identity provider",
        ));
    }
    let start = state
        .services
        .oidc
        .start_login(Some(return_to), q.idp)
        .await?;
    let sealed = state.services.tokens.seal(
        "login",
        &serde_json::to_value(&start).map_err(|e| Error::Internal(e.to_string()))?,
        LOGIN_SECONDS,
    )?;
    let c = set_cookie(
        &state,
        LOGIN_COOKIE,
        &sealed,
        "/v1/auth",
        LOGIN_SECONDS,
        true,
    );
    redirect(&start.authorize_url, &[c])
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// OIDC callback. `GET /v1/auth/callback` (operationId `loginCallback`).
pub async fn login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    QueryParams(q): QueryParams<CallbackQuery>,
) -> ApiResult<Response> {
    if let Some(e) = q.error {
        return Err(HttpError(Error::Api(
            ApiError::new(
                ErrorCode::Unauthenticated,
                format!("the identity provider refused the sign-in: {e}"),
            )
            .with_hint("start the sign-in again at /v1/auth/login"),
        )));
    }
    let sealed = cookie(&headers, LOGIN_COOKIE).ok_or_else(|| {
        bad_request(
            "the sign-in state cookie is missing",
            "start the sign-in again at /v1/auth/login",
        )
    })?;
    let start: LoginStart = state
        .services
        .tokens
        .open("login", &sealed)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| {
            bad_request(
                "the sign-in state is not valid or has expired",
                "start the sign-in again at /v1/auth/login",
            )
        })?;
    let state_param = q.state.unwrap_or_default();
    if !secret::constant_time_eq(state_param.as_bytes(), start.state.as_bytes()) {
        return Err(bad_request(
            "the sign-in state does not match",
            "start the sign-in again at /v1/auth/login",
        ));
    }
    let code = q
        .code
        .filter(|c| !c.is_empty())
        .ok_or_else(|| bad_request("the code is missing", "start the sign-in again"))?;
    let login = state.services.oidc.finish_login(&code, &start).await?;
    let user = state.services.identity.upsert_from_login(&login).await?;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (_session, session_secret) = state
        .services
        .identity
        .create_session(user.id, user_agent)
        .await?;
    let csrf = secret::random_secret("");
    let max_age = SESSION_DAYS * 24 * 3600;
    let return_to = if valid_return_to(&start.return_to) {
        start.return_to
    } else {
        "/".into()
    };
    redirect(
        &return_to,
        &[
            set_cookie(&state, SESSION_COOKIE, &session_secret, "/", max_age, true),
            set_cookie(&state, CSRF_COOKIE, &csrf, "/", max_age, false),
            set_cookie(&state, LOGIN_COOKIE, "", "/v1/auth", 0, true),
        ],
    )
}

/// Sign out. `POST /v1/auth/logout` (operationId `logout`).
pub async fn logout(
    State(state): State<AppState>,
    Principal(who): Principal,
) -> ApiResult<Response> {
    refuse_share_link(&who)?;
    if let (Some(session), Some(user)) = (who.session_id, who.user()) {
        state.services.identity.end_session(user, session).await?;
    }
    let logout_url = state.services.oidc.logout_url(None).await?;
    let mut res = Json(json!({ "logout_url": logout_url })).into_response();
    for c in [
        set_cookie(&state, SESSION_COOKIE, "", "/", 0, true),
        set_cookie(&state, CSRF_COOKIE, "", "/", 0, false),
    ] {
        if let Ok(v) = HeaderValue::from_str(&c) {
            res.headers_mut().append(SET_COOKIE, v);
        }
    }
    Ok(res)
}

/// The signed-in principal. `GET /v1/me` (operationId `getMe`).
pub async fn get_me(
    State(state): State<AppState>,
    Principal(who): Principal,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&who)?;
    let user_id = who.user().ok_or_else(Error::user_required)?;
    let user = state.services.identity.get_user(user_id).await?;
    let agent = match (who.actor.actor.kind, who.bound_tenant) {
        (PrincipalKind::Agent, Some(t)) => Some(
            state
                .services
                .agents
                .authenticate(t, ruum_core::AgentId(who.actor.actor.id.0))
                .await?,
        ),
        _ => None,
    };
    Ok(Json(json!({
        "actor": who.actor.actor,
        "on_behalf_of": who.actor.on_behalf_of,
        "user": user,
        "agent": agent,
        "bound_tenant_id": who.bound_tenant,
    })))
}

/// Linked identities of the signed-in user. `GET /v1/me/identities` (operationId `listMyIdentities`).
pub async fn list_my_identities(
    State(state): State<AppState>,
    Principal(who): Principal,
) -> ApiResult<Json<Value>> {
    let user = require_user(&who)?;
    let items = state.services.identity.identities_of(user).await?;
    Ok(Json(json!({ "items": items })))
}

/// Web sessions of the signed-in user. `GET /v1/me/sessions` (operationId `listMySessions`).
pub async fn list_my_sessions(
    State(state): State<AppState>,
    Principal(who): Principal,
) -> ApiResult<Json<Value>> {
    let user = require_user(&who)?;
    let sessions = state.services.identity.sessions_of(user).await?;
    let items: Vec<Value> = sessions
        .into_iter()
        .map(|s| {
            json!({
                "id": s.id,
                "created_at": rfc3339(s.created_at),
                "expires_at": rfc3339(s.expires_at),
                "last_seen_at": rfc3339(s.last_seen_at),
                "user_agent": s.user_agent,
                "current": who.session_id == Some(s.id),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// End one web session. `DELETE /v1/me/sessions/{session_id}` (operationId `endSession`).
pub async fn end_session(
    State(state): State<AppState>,
    Principal(who): Principal,
    Path(session_id): Path<String>,
) -> ApiResult<StatusCode> {
    let user = require_user(&who)?;
    let id: Uuid = session_id
        .parse()
        .map_err(|_| HttpError(Error::not_found("session")))?;
    state.services.identity.end_session(user, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// App passwords of the signed-in user in one tenant. `GET /v1/me/app-passwords` (operationId
/// `listAppPasswords`).
pub async fn list_app_passwords(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
) -> ApiResult<Json<Value>> {
    let items = state.services.identity.list_app_passwords(&ctx).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct CreateAppPassword {
    name: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

/// Create a WebDAV app password. `POST /v1/me/app-passwords` (operationId `createAppPassword`).
pub async fn create_app_password(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    JsonBody(body): JsonBody<CreateAppPassword>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let (pw, secret) = state
        .services
        .identity
        .create_app_password(&ctx, body.name, body.expires_at)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "app_password": pw, "secret": secret })),
    ))
}

/// Revoke an app password. `DELETE /v1/me/app-passwords/{app_password_id}` (operationId
/// `revokeAppPassword`).
pub async fn revoke_app_password(
    State(state): State<AppState>,
    RequestContext(ctx): RequestContext,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    ruum_core::actor::require_user_actor(&ctx)?;
    let id: AppPasswordId = id
        .parse()
        .map_err(|_| HttpError(Error::not_found("app password")))?;
    state
        .services
        .identity
        .revoke_app_password(&ctx, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::{AppState, router};
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use ruum_core::Services;
    use ruum_core::agents::memory::MemoryAgentStore;
    use ruum_core::agents::oauth::MemoryOAuthServer;
    use ruum_core::identity::VerifiedLogin;
    use ruum_core::identity::memory::{
        MemoryArea, MemoryDeps, MemoryIdentityStore, MemoryOidcClient,
    };
    use ruum_core::tenants::memory::MemoryTenantStore;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt;

    /// An app with the identity area alone (no authz, journal or audit), and the fake provider.
    pub(crate) fn app() -> (axum::Router, Arc<MemoryOidcClient>, AppState) {
        let mut s = Services::in_memory();
        let oidc = Arc::new(MemoryOidcClient::default());
        let area = Arc::new(MemoryArea::new(MemoryDeps {
            tokens: s.tokens.clone(),
            ..MemoryDeps::default()
        }));
        s.identity = Arc::new(MemoryIdentityStore::new(area.clone()));
        s.tenants = Arc::new(MemoryTenantStore::new(area.clone()));
        s.agents = Arc::new(MemoryAgentStore::new(area.clone()));
        s.oauth = Arc::new(MemoryOAuthServer::new(area));
        s.oidc = oidc.clone();
        let state = AppState::new(s, crate::ApiConfig::default());
        (router(state.clone()), oidc, state)
    }

    pub(crate) struct Res {
        pub status: StatusCode,
        pub headers: axum::http::HeaderMap,
        pub body: Value,
    }

    pub(crate) async fn call(
        app: &axum::Router,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Res {
        let mut b = Request::builder().method(method).uri(uri);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                b = b.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let res = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Res {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        }
    }

    fn cookie_value(res: &Res, name: &str) -> String {
        res.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| c.strip_prefix(&format!("{name}=")))
            .map(|c| c.split(';').next().unwrap().to_string())
            .unwrap_or_default()
    }

    /// Signs in through the fake provider and returns the session and CSRF cookie values.
    pub(crate) async fn sign_in(
        app: &axum::Router,
        oidc: &MemoryOidcClient,
        email: &str,
    ) -> (String, String) {
        let start = call(
            app,
            Method::GET,
            "/v1/auth/login?return_to=%2Ffiles",
            &[],
            None,
        )
        .await;
        assert_eq!(start.status, StatusCode::FOUND);
        let location = start.headers["location"].to_str().unwrap().to_string();
        let state = url::Url::parse(&location)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let login_cookie = cookie_value(&start, "ruum_login");
        oidc.add_login(
            "code-1",
            VerifiedLogin {
                issuer: MemoryOidcClient::ISSUER.into(),
                subject: email.into(),
                email: Some(email.into()),
                email_verified: true,
                name: Some("Mari".into()),
                id_token: None,
            },
        );
        // A wrong state is refused.
        let bad = call(
            app,
            Method::GET,
            "/v1/auth/callback?code=code-1&state=wrong",
            &[("cookie", &format!("ruum_login={login_cookie}"))],
            None,
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        let cb = call(
            app,
            Method::GET,
            &format!("/v1/auth/callback?code=code-1&state={state}"),
            &[("cookie", &format!("ruum_login={login_cookie}"))],
            None,
        )
        .await;
        assert_eq!(cb.status, StatusCode::FOUND);
        assert_eq!(cb.headers["location"], "/files");
        let set: Vec<&str> = cb
            .headers
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap())
            .collect();
        assert!(set.iter().any(|c| c.starts_with("ruum_session=")
            && c.contains("HttpOnly")
            && c.contains("Secure")));
        (
            cookie_value(&cb, "ruum_session"),
            cookie_value(&cb, "ruum_csrf"),
        )
    }

    #[tokio::test]
    async fn sign_in_session_csrf_and_logout() {
        let (app, oidc, _) = app();
        let (session, csrf) = sign_in(&app, &oidc, "mari@example.test").await;
        let cookie = format!("ruum_session={session}; ruum_csrf={csrf}");
        let me = call(&app, Method::GET, "/v1/me", &[("cookie", &cookie)], None).await;
        assert_eq!(me.status, StatusCode::OK);
        assert_eq!(me.body["user"]["email"], "mari@example.test");
        assert_eq!(me.body["actor"]["kind"], "user");
        let tenants = call(
            &app,
            Method::GET,
            "/v1/tenants",
            &[("cookie", &cookie)],
            None,
        )
        .await;
        assert_eq!(tenants.body["items"][0]["role"], "owner");
        let sessions = call(
            &app,
            Method::GET,
            "/v1/me/sessions",
            &[("cookie", &cookie)],
            None,
        )
        .await;
        assert_eq!(sessions.body["items"][0]["current"], true);
        // A write without the CSRF header is refused.
        let no_csrf = call(
            &app,
            Method::POST,
            "/v1/auth/logout",
            &[("cookie", &cookie)],
            None,
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
        let out = call(
            &app,
            Method::POST,
            "/v1/auth/logout",
            &[("cookie", &cookie), ("ruum-csrf", &csrf)],
            None,
        )
        .await;
        assert_eq!(out.status, StatusCode::OK);
        assert!(out.body["logout_url"].is_string());
        let after = call(&app, Method::GET, "/v1/me", &[("cookie", &cookie)], None).await;
        assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unauthenticated_calls_point_to_the_resource_metadata() {
        let (app, _, _) = app();
        let r = call(&app, Method::GET, "/v1/me", &[], None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
        assert_eq!(r.body["error"]["code"], "unauthenticated");
        assert_eq!(
            r.headers["www-authenticate"],
            "Bearer resource_metadata=\"https://ruum.localhost/.well-known/oauth-protected-resource\""
        );
        let bad = call(
            &app,
            Method::GET,
            "/v1/me",
            &[("authorization", "Bearer nope")],
            None,
        )
        .await;
        assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
        let key = call(
            &app,
            Method::GET,
            "/v1/me",
            &[("authorization", "Bearer ruum_agent_xyz")],
            None,
        )
        .await;
        assert_eq!(key.status, StatusCode::UNAUTHORIZED);
        assert!(
            key.body["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("exchange")
        );
        let jwks = call(&app, Method::GET, "/.well-known/jwks.json", &[], None).await;
        assert_eq!(jwks.body["keys"][0]["kty"], "OKP");
        let meta = call(
            &app,
            Method::GET,
            "/.well-known/oauth-protected-resource",
            &[],
            None,
        )
        .await;
        assert_eq!(meta.body["resource"], "https://ruum.localhost");
    }

    #[tokio::test]
    async fn agents_keys_tokens_and_idempotency_over_http() {
        let (app, oidc, _) = app();
        let (session, csrf) = sign_in(&app, &oidc, "kati@example.test").await;
        let cookie = format!("ruum_session={session}; ruum_csrf={csrf}");
        let me = call(
            &app,
            Method::GET,
            "/v1/tenants",
            &[("cookie", &cookie)],
            None,
        )
        .await;
        let tid = me.body["items"][0]["tenant"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let user_headers = [
            ("cookie", cookie.as_str()),
            ("ruum-csrf", csrf.as_str()),
            ("ruum-tenant", tid.as_str()),
            ("idempotency-key", "register-agent-1"),
        ];
        let body = json!({ "name": "bot", "scope": { "subtree": null, "max_role": "edit" } });
        let a1 = call(
            &app,
            Method::POST,
            "/v1/agents",
            &user_headers,
            Some(body.clone()),
        )
        .await;
        assert_eq!(a1.status, StatusCode::CREATED, "{}", a1.body);
        // The retry gets the first answer; another body with the same key is refused.
        let a2 = call(&app, Method::POST, "/v1/agents", &user_headers, Some(body)).await;
        assert_eq!(a2.status, StatusCode::CREATED);
        assert_eq!(a2.body["id"], a1.body["id"]);
        assert_eq!(a2.headers["idempotent-replayed"], "true");
        let a3 = call(
            &app,
            Method::POST,
            "/v1/agents",
            &user_headers,
            Some(json!({ "name": "other", "scope": { "subtree": null, "max_role": "view" } })),
        )
        .await;
        assert_eq!(a3.status, StatusCode::CONFLICT);
        assert_eq!(a3.body["error"]["code"], "idempotency_key_reused");
        let agent = a1.body["id"].as_str().unwrap().to_string();
        let headers = &user_headers[..3];
        let key = call(
            &app,
            Method::POST,
            &format!("/v1/agents/{agent}/keys"),
            headers,
            Some(json!({})),
        )
        .await;
        assert_eq!(key.status, StatusCode::CREATED);
        let secret = key.body["secret"].as_str().unwrap().to_string();
        // A key made with an Idempotency-Key: the secret is not kept, so the retry gets 409
        // and never sees the secret again.
        let key_headers = [
            headers[0],
            headers[1],
            headers[2],
            ("idempotency-key", "create-key-0001"),
        ];
        let k1 = call(
            &app,
            Method::POST,
            &format!("/v1/agents/{agent}/keys"),
            &key_headers,
            Some(json!({ "name": "ci" })),
        )
        .await;
        assert_eq!(k1.status, StatusCode::CREATED, "{}", k1.body);
        let k1_secret = k1.body["secret"].as_str().unwrap().to_string();
        let k2 = call(
            &app,
            Method::POST,
            &format!("/v1/agents/{agent}/keys"),
            &key_headers,
            Some(json!({ "name": "ci" })),
        )
        .await;
        assert_eq!(k2.status, StatusCode::CONFLICT, "{}", k2.body);
        assert_eq!(k2.body["error"]["code"], "conflict");
        assert!(!k2.body.to_string().contains("ruum_agent_"));
        assert!(!k2.body.to_string().contains(&k1_secret));
        // The key exchange at the token endpoint (form body).
        let form = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange&subject_token={secret}\
             &subject_token_type=urn%3Aruum%3Aparams%3Aoauth%3Atoken-type%3Aapi-key"
        );
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/oauth/token")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["cache-control"], "no-store");
        let tok: Value =
            serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let access = tok["access_token"].as_str().unwrap().to_string();
        let bearer = format!("Bearer {access}");
        // The agent is itself in /v1/me, bound to the tenant; it cannot make keys.
        let me = call(
            &app,
            Method::GET,
            "/v1/me",
            &[("authorization", &bearer)],
            None,
        )
        .await;
        assert_eq!(me.body["actor"]["kind"], "agent");
        assert_eq!(me.body["bound_tenant_id"], tid.as_str());
        let mint = call(
            &app,
            Method::POST,
            &format!("/v1/agents/{agent}/keys"),
            &[("authorization", &bearer)],
            Some(json!({})),
        )
        .await;
        assert_eq!(mint.status, StatusCode::FORBIDDEN);
        assert_eq!(mint.body["error"]["code"], "user_required");
        // Disable: the token stops at once.
        let off = call(
            &app,
            Method::POST,
            &format!("/v1/agents/{agent}/disable"),
            headers,
            None,
        )
        .await;
        assert_eq!(off.status, StatusCode::OK);
        let me = call(
            &app,
            Method::GET,
            "/v1/me",
            &[("authorization", &bearer)],
            None,
        )
        .await;
        assert_eq!(me.status, StatusCode::FORBIDDEN);
        assert_eq!(me.body["error"]["code"], "agent_disabled");
        // A bad grant type is an OAuth error.
        let bad = call(
            &app,
            Method::POST,
            "/v1/oauth/token",
            &[],
            Some(json!({ "grant_type": "password" })),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert_eq!(bad.body["error"], "unsupported_grant_type");
    }

    #[tokio::test]
    async fn oauth_authorize_redirects_or_shows_400() {
        let (app, _, _) = app();
        let reg = call(
            &app,
            Method::POST,
            "/v1/oauth/register",
            &[],
            Some(json!({ "client_name": "Claude", "redirect_uris": ["http://127.0.0.1:9999/cb"] })),
        )
        .await;
        assert_eq!(reg.status, StatusCode::CREATED);
        let client = reg.body["client_id"].as_str().unwrap();
        let ok = call(
            &app,
            Method::GET,
            &format!(
                "/v1/oauth/authorize?response_type=code&client_id={client}\
                 &redirect_uri=http%3A%2F%2F127.0.0.1%3A9999%2Fcb\
                 &code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM&code_challenge_method=S256&state=s"
            ),
            &[],
            None,
        )
        .await;
        assert_eq!(ok.status, StatusCode::FOUND);
        let loc = ok.headers["location"].to_str().unwrap();
        assert!(
            loc.starts_with("/v1/auth/login?return_to=%2Foauth%2Fconsent%3Frequest%3D"),
            "{loc}"
        );
        let plain = call(
            &app,
            Method::GET,
            &format!(
                "/v1/oauth/authorize?response_type=code&client_id={client}\
                 &redirect_uri=http%3A%2F%2F127.0.0.1%3A9999%2Fcb&code_challenge_method=plain&state=s"
            ),
            &[],
            None,
        )
        .await;
        assert_eq!(plain.status, StatusCode::FOUND);
        assert!(
            plain.headers["location"]
                .to_str()
                .unwrap()
                .contains("error=invalid_request")
        );
        let unknown = call(
            &app,
            Method::GET,
            "/v1/oauth/authorize?response_type=code&client_id=nope&redirect_uri=http%3A%2F%2Fevil.example%2F",
            &[],
            None,
        )
        .await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
        let bad = call(
            &app,
            Method::POST,
            "/v1/oauth/register",
            &[],
            Some(json!({ "client_name": "x", "redirect_uris": ["http://evil.example/cb"] })),
        )
        .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert_eq!(bad.body["error"], "invalid_redirect_uri");
    }
}
