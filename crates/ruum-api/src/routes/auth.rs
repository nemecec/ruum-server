//! Routes of the `auth` area: sign-in through Zitadel (OIDC code flow with PKCE), sessions, the
//! signed-in principal and WebDAV app passwords. See `docs/STEP1.md` for the owner of this file.

use crate::auth::{
    JsonBody, Principal, QueryParams, RequestContext, cookie, current_client, refuse_share_link,
    require_user,
};
use crate::error::{ApiResult, HttpError};
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::{LOCATION, SET_COOKIE, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use ruum_core::audit::events;
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
        .route("/v1/me/preferences", patch(update_my_preferences))
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

/// A sign-in that failed before a known user was found (bad state, bad code, an error of the
/// identity provider). It goes to no tenant: a row there would let anyone write into a victim's
/// log (decision 0003-4). It is a `tracing` security event with a short reason code.
fn login_refused(reason: &'static str, error: HttpError) -> HttpError {
    tracing::warn!(target: "ruum::security", event = "auth.login_failed", reason, "a sign-in failed");
    error
}

/// OIDC callback. `GET /v1/auth/callback` (operationId `loginCallback`).
///
/// A sign-in writes `auth.login` (IP, user agent, the `sessions.id` of the new session) in the
/// user's home tenant, and nowhere else (decision 0003-4); other tenants get their row at the
/// first use of the session there ([`crate::auth::note_session_use`]). The write is
/// synchronous: when it fails, the new session ends and the sign-in fails (design 15).
pub async fn login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    QueryParams(q): QueryParams<CallbackQuery>,
) -> ApiResult<Response> {
    if let Some(e) = q.error {
        return Err(login_refused(
            "provider_error",
            HttpError(Error::Api(
                ApiError::new(
                    ErrorCode::Unauthenticated,
                    format!("the identity provider refused the sign-in: {e}"),
                )
                .with_hint("start the sign-in again at /v1/auth/login"),
            )),
        ));
    }
    let sealed = cookie(&headers, LOGIN_COOKIE).ok_or_else(|| {
        login_refused(
            "state_missing",
            bad_request(
                "the sign-in state cookie is missing",
                "start the sign-in again at /v1/auth/login",
            ),
        )
    })?;
    let start: LoginStart = state
        .services
        .tokens
        .open("login", &sealed)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| {
            login_refused(
                "state_invalid",
                bad_request(
                    "the sign-in state is not valid or has expired",
                    "start the sign-in again at /v1/auth/login",
                ),
            )
        })?;
    let state_param = q.state.unwrap_or_default();
    if !secret::constant_time_eq(state_param.as_bytes(), start.state.as_bytes()) {
        return Err(login_refused(
            "state_mismatch",
            bad_request(
                "the sign-in state does not match",
                "start the sign-in again at /v1/auth/login",
            ),
        ));
    }
    let code = q.code.filter(|c| !c.is_empty()).ok_or_else(|| {
        login_refused(
            "code_missing",
            bad_request("the code is missing", "start the sign-in again"),
        )
    })?;
    let login = state
        .services
        .oidc
        .finish_login(&code, &start)
        .await
        .map_err(|e| login_refused("code_refused", HttpError(e)))?;
    // The allow-list of a closed server (staging, review 1 B-5): an address that is not on it
    // gets no user, no personal tenant and no session. The check runs before the upsert, so
    // nothing is written for a refused address.
    if let Some(list) = &state.config.signup
        && !list.allows(login.email.as_deref(), login.email_verified)
    {
        return Err(login_refused(
            "email_not_allowed",
            HttpError(Error::Api(
                ApiError::new(
                    ErrorCode::Unauthenticated,
                    "this server is open only to invited addresses, and yours is not one of them",
                )
                .with_hint("ask the operator to add your address to the allow-list"),
            )),
        ));
    }
    let user = state.services.identity.upsert_from_login(&login).await?;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let (session, session_secret) = state
        .services
        .identity
        .create_session(user.id, user_agent)
        .await?;
    if let Err(e) = events::sign_in(
        state.services.audit.as_ref(),
        user.home_tenant_id,
        user.id,
        "oidc",
        Some(session.id),
        &current_client(),
    )
    .await
    {
        // Fail closed: no audit row, no session.
        if let Err(end) = state
            .services
            .identity
            .end_session(user.id, session.id)
            .await
        {
            tracing::warn!(error = %end, "could not end the session of a failed sign-in");
        }
        return Err(HttpError(e));
    }
    // Every sign-in binds the email grants of the user's verified email (decision 0003-2
    // (b)). A failure does not fail the sign-in; the grants stay email grants.
    if let Err(e) = state
        .services
        .identity
        .bind_email_grants(user.id, session.id, true)
        .await
    {
        tracing::warn!(error = %e, user = %user.id, "could not bind email grants");
    }
    // Bound to the session (decision 0002-3): the check recomputes it from the session cookie.
    let csrf = ruum_core::identity::csrf_token(&session_secret);
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
        // `auth.logout` in the home tenant first: a failed write fails the sign-out (design 15).
        let home = state.services.identity.get_user(user).await?.home_tenant_id;
        events::sign_out(state.services.audit.as_ref(), home, user, &current_client()).await?;
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

/// Binds the pending email grants of a web session's user (decision 0003-2 (b)): only for a
/// user signed in with the session cookie, never for agent tokens, app passwords, share-link
/// tokens or bearer tokens. `force` skips the 60-second throttle. A failure is logged; the
/// request goes on (the grants stay email grants).
pub(crate) async fn bind_for_session(
    state: &AppState,
    who: &ruum_core::identity::Authenticated,
    force: bool,
) {
    let (Some(session), PrincipalKind::User, None) =
        (who.session_id, who.actor.actor.kind, who.bound_tenant)
    else {
        return;
    };
    let user = ruum_core::UserId(who.actor.actor.id.0);
    if let Err(e) = state
        .services
        .identity
        .bind_email_grants(user, session, force)
        .await
    {
        tracing::warn!(error = %e, %user, "could not bind email grants");
    }
}

/// The signed-in principal. `GET /v1/me` (operationId `getMe`).
pub async fn get_me(
    State(state): State<AppState>,
    Principal(who): Principal,
) -> ApiResult<Json<Value>> {
    refuse_share_link(&who)?;
    let user_id = who.user().ok_or_else(Error::user_required)?;
    bind_for_session(&state, &who, false).await;
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
    let mut me = json!({
        "actor": who.actor.actor,
        "on_behalf_of": who.actor.on_behalf_of,
        "user": user,
        "agent": agent,
        "bound_tenant_id": who.bound_tenant,
    });
    // The preferences are the user's own: an agent does not get them.
    if let Ok(uid) = require_user(&who) {
        me["preferences"] = json!(state.services.identity.preferences_of(uid).await?);
    }
    Ok(Json(me))
}

/// A field that may be absent (`None`), `null` (`Some(None)`) or a value.
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePreferences {
    #[serde(default, deserialize_with = "present")]
    locale: Option<Option<String>>,
}

/// Change the preferences of the signed-in user. `PATCH /v1/me/preferences` (operationId
/// `updateMyPreferences`). Users only.
pub async fn update_my_preferences(
    State(state): State<AppState>,
    Principal(who): Principal,
    JsonBody(body): JsonBody<UpdatePreferences>,
) -> ApiResult<Json<Value>> {
    let user = require_user(&who)?;
    let identity = &state.services.identity;
    let prefs = match body.locale {
        Some(locale) => {
            ruum_core::identity::check_locale(locale.as_deref())?;
            identity.set_locale(user, locale).await?
        }
        None => identity.preferences_of(user).await?,
    };
    Ok(Json(json!(prefs)))
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
    // Another user's session is `not_found` here, before any audit row.
    state.services.identity.end_session(user, id).await?;
    let home = state.services.identity.get_user(user).await?.home_tenant_id;
    events::sign_out(state.services.audit.as_ref(), home, user, &current_client()).await?;
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
        Json(json!({
            "app_password": pw,
            "secret": secret,
            "dav_url": state.config.dav_url,
        })),
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

    /// The language preference (design 12.4): `getMe` returns it, `updateMyPreferences` sets
    /// it, for the caller only.
    #[tokio::test]
    async fn preferences_are_the_users_own() {
        let (app, oidc, _) = app();
        let (session, csrf) = sign_in(&app, &oidc, "mari@example.test").await;
        let cookie = format!("ruum_session={session}; ruum_csrf={csrf}");
        let mine: &[(&str, &str)] = &[("cookie", &cookie), ("ruum-csrf", &csrf)];
        let me = call(&app, Method::GET, "/v1/me", &[("cookie", &cookie)], None).await;
        assert_eq!(me.body["preferences"], json!({ "locale": null }));
        // A write without the CSRF header is refused.
        let no_csrf = call(
            &app,
            Method::PATCH,
            "/v1/me/preferences",
            &[("cookie", &cookie)],
            Some(json!({ "locale": "et" })),
        )
        .await;
        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
        let set = call(
            &app,
            Method::PATCH,
            "/v1/me/preferences",
            mine,
            Some(json!({ "locale": "et" })),
        )
        .await;
        assert_eq!(set.status, StatusCode::OK, "{}", set.body);
        assert_eq!(set.body, json!({ "locale": "et" }));
        let me = call(&app, Method::GET, "/v1/me", &[("cookie", &cookie)], None).await;
        assert_eq!(me.body["preferences"]["locale"], "et");
        // Left out, it stays.
        let keep = call(
            &app,
            Method::PATCH,
            "/v1/me/preferences",
            mine,
            Some(json!({})),
        )
        .await;
        assert_eq!(keep.body["locale"], "et");
        // A tag that is not BCP 47, or an unknown field, is refused and changes nothing.
        for bad in [
            json!({ "locale": "Estonian" }),
            json!({ "locale": "et_EE" }),
        ] {
            let r = call(&app, Method::PATCH, "/v1/me/preferences", mine, Some(bad)).await;
            assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
            assert_eq!(r.body["error"]["code"], "validation_failed");
        }
        let unknown = call(
            &app,
            Method::PATCH,
            "/v1/me/preferences",
            mine,
            Some(json!({ "theme": "dark" })),
        )
        .await;
        assert!(unknown.status.is_client_error(), "{}", unknown.status);
        // Another user does not see or change Mari's preference.
        let (s2, c2) = sign_in(&app, &oidc, "jaan@example.test").await;
        let cookie2 = format!("ruum_session={s2}; ruum_csrf={c2}");
        let other = call(&app, Method::GET, "/v1/me", &[("cookie", &cookie2)], None).await;
        assert_eq!(other.body["preferences"]["locale"], Value::Null);
        let me = call(&app, Method::GET, "/v1/me", &[("cookie", &cookie)], None).await;
        assert_eq!(me.body["preferences"]["locale"], "et");
        // `null` clears it.
        let clear = call(
            &app,
            Method::PATCH,
            "/v1/me/preferences",
            mine,
            Some(json!({ "locale": null })),
        )
        .await;
        assert_eq!(clear.body, json!({ "locale": null }));
    }

    /// The CSRF value is bound to the session (decision 0002-3): a header and cookie pair that
    /// match each other but were not issued for this session fail, as after cookie tossing from
    /// a sibling host.
    #[tokio::test]
    async fn the_csrf_value_is_bound_to_the_session() {
        let (app, oidc, _) = app();
        let (session_a, csrf_a) = sign_in(&app, &oidc, "anna@example.test").await;
        let (_session_b, csrf_b) = sign_in(&app, &oidc, "berit@example.test").await;
        assert_ne!(csrf_a, csrf_b);
        let tossed = "attacker-chosen-value".to_string();
        for forged in [&csrf_b, &tossed] {
            let cookie = format!("ruum_session={session_a}; ruum_csrf={forged}");
            let r = call(
                &app,
                Method::POST,
                "/v1/auth/logout",
                &[("cookie", &cookie), ("ruum-csrf", forged)],
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{forged}");
            assert_eq!(r.body["error"]["code"], "permission_denied");
        }
        // The session still works, and its own value passes, also next to a tossed cookie.
        let cookie = format!("ruum_session={session_a}; ruum_csrf={tossed}");
        let ok = call(
            &app,
            Method::POST,
            "/v1/auth/logout",
            &[("cookie", &cookie), ("ruum-csrf", &csrf_a)],
            None,
        )
        .await;
        assert_eq!(ok.status, StatusCode::OK);
    }

    /// The tenant's freshness token cannot be read (the database is down): the request fails,
    /// it never goes on with a stale authz read (decision 0002-4).
    #[tokio::test]
    async fn a_failed_freshness_lookup_fails_the_request() {
        struct Broken;
        #[async_trait::async_trait]
        impl ruum_core::authz::Freshness for Broken {
            async fn tenant_token(
                &self,
                _: ruum_core::TenantId,
            ) -> ruum_core::Result<Option<ruum_core::authz::ConsistencyToken>> {
                Err(ruum_core::Error::Internal(
                    "the outbox cannot be read".into(),
                ))
            }
        }
        let (_, oidc, state) = app();
        let mut services = state.services.clone();
        services.freshness = Arc::new(Broken);
        let app = router(AppState::new(services, state.config.clone()));
        let (session, _csrf) = sign_in(&app, &oidc, "erik@example.test").await;
        let cookie = format!("ruum_session={session}");
        // Without a tenant there is nothing to read.
        let tenants = call(
            &app,
            Method::GET,
            "/v1/tenants",
            &[("cookie", &cookie)],
            None,
        )
        .await;
        assert_eq!(tenants.status, StatusCode::OK);
        let tenant = tenants.body["items"][0]["tenant"]["id"]
            .as_str()
            .or_else(|| tenants.body["items"][0]["id"].as_str())
            .expect("a tenant id")
            .to_string();
        for path in ["/v1/agents", "/v1/nodes/root", "/v1/trash"] {
            let r = call(
                &app,
                Method::GET,
                path,
                &[("cookie", &cookie), ("ruum-tenant", &tenant)],
                None,
            )
            .await;
            assert_eq!(
                r.status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{path}: {}",
                r.body
            );
        }
    }

    /// A new app password comes with the URL of the WebDAV host to mount (decision 0002-3).
    #[tokio::test]
    async fn a_new_app_password_names_the_dav_host() {
        let (app, oidc, _) = app();
        let (session, csrf) = sign_in(&app, &oidc, "dora@example.test").await;
        let cookie = format!("ruum_session={session}; ruum_csrf={csrf}");
        let tenants = call(
            &app,
            Method::GET,
            "/v1/tenants",
            &[("cookie", &cookie)],
            None,
        )
        .await;
        let tenant = tenants.body["items"][0]["tenant"]["id"]
            .as_str()
            .or_else(|| tenants.body["items"][0]["id"].as_str())
            .expect("a tenant id")
            .to_string();
        let r = call(
            &app,
            Method::POST,
            "/v1/me/app-passwords",
            &[
                ("cookie", &cookie),
                ("ruum-csrf", &csrf),
                ("ruum-tenant", &tenant),
            ],
            Some(serde_json::json!({ "name": "Finder" })),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        assert_eq!(r.body["dav_url"], "https://dav.ruum.localhost/");
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
        // The user's preferences are not the agent's to read or set.
        assert!(me.body.get("preferences").is_none(), "{}", me.body);
        let prefs = call(
            &app,
            Method::PATCH,
            "/v1/me/preferences",
            &[("authorization", &bearer)],
            Some(json!({ "locale": "et" })),
        )
        .await;
        assert_eq!(prefs.status, StatusCode::FORBIDDEN);
        assert_eq!(prefs.body["error"]["code"], "user_required");
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
        // The hint names the page of the web UI with the public URL (decision 0003-7, fix 6).
        assert_eq!(
            mint.body["error"]["hint"],
            "ask the user to do this in the Ruum web UI at https://ruum.localhost/agents"
        );
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

    /// The audit entries of `tenant`, oldest first.
    async fn audit_of(state: &AppState, tenant: ruum_core::TenantId) -> Vec<Value> {
        let ctx = ruum_core::Context {
            tenant,
            actor: ruum_core::Actor::user(ruum_core::UserId::new()),
            scope: None,
            idempotency_key: None,
        };
        let mut items = state
            .services
            .audit
            .list(&ctx, ruum_core::audit::AuditQuery::default())
            .await
            .unwrap()
            .items;
        items.reverse();
        items.into_iter().map(|e| json!(e)).collect()
    }

    /// Signs in and returns the cookie header, the CSRF value, the user ID and the home tenant.
    async fn signed_in(
        app: &axum::Router,
        oidc: &MemoryOidcClient,
        email: &str,
    ) -> (String, String, ruum_core::UserId, ruum_core::TenantId) {
        let (session, csrf) = sign_in(app, oidc, email).await;
        let cookie = format!("ruum_session={session}; ruum_csrf={csrf}");
        let me = call(app, Method::GET, "/v1/me", &[("cookie", &cookie)], None).await;
        let user: ruum_core::UserId = me.body["user"]["id"].as_str().unwrap().parse().unwrap();
        let home: ruum_core::TenantId = me.body["user"]["home_tenant_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        (cookie, csrf, user, home)
    }

    /// Decision 0003-4 (C-14): a sign-in writes `auth.login` in the home tenant only; the first
    /// use of the session in a team tenant (as a member) writes one `auth.login` there with
    /// `via: session_first_use`; later requests write none; a new session writes again; a guest
    /// gets no row in the host tenant; sign-out writes `auth.logout` in the home tenant.
    #[tokio::test]
    async fn sign_ins_are_audited_in_the_home_tenant_and_at_first_use() {
        use ruum_core::tenants::MemberRole;
        let (app, oidc, state) = app();
        let (owner_cookie, _, owner, owner_home) =
            signed_in(&app, &oidc, "owner@example.test").await;
        let team = state
            .services
            .tenants
            .create_team(&ruum_core::Actor::user(owner), "Team".into())
            .await
            .unwrap();
        let owner_ctx = ruum_core::Context {
            tenant: team.id,
            actor: ruum_core::Actor::user(owner),
            scope: None,
            idempotency_key: None,
        };
        let (member_cookie, member_csrf, member, member_home) =
            signed_in(&app, &oidc, "member@example.test").await;
        let (guest_cookie, _, guest, _) = signed_in(&app, &oidc, "guest@example.test").await;
        for (who, email, role) in [
            (member, "member@example.test", MemberRole::Member),
            (guest, "guest@example.test", MemberRole::Guest),
        ] {
            let (_, token) = state
                .services
                .tenants
                .invite(&owner_ctx, email.into(), role, None)
                .await
                .unwrap();
            state
                .services
                .tenants
                .accept_invitation(&ruum_core::Actor::user(who), &token)
                .await
                .unwrap();
        }

        // The sign-in wrote only in the home tenant, with the session row ID.
        let home = audit_of(&state, member_home).await;
        assert_eq!(home.len(), 1, "{home:?}");
        assert_eq!(home[0]["action"], "auth.login");
        assert_eq!(home[0]["actor"]["id"], json!(member));
        assert!(home[0]["data"]["session_id"].is_string());
        assert_eq!(audit_of(&state, owner_home).await.len(), 1);
        let logins = |v: &[Value]| v.iter().filter(|e| e["action"] == "auth.login").count();
        assert_eq!(logins(&audit_of(&state, team.id).await), 0);

        // The first use of the member's session in the team writes one row; the next none.
        let team_id = team.id.to_string();
        for _ in 0..2 {
            let r = call(
                &app,
                Method::GET,
                "/v1/me/app-passwords",
                &[("cookie", &member_cookie), ("ruum-tenant", &team_id)],
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::OK);
        }
        let r = call(
            &app,
            Method::GET,
            &format!("/v1/tenants/{team_id}"),
            &[("cookie", &member_cookie), ("user-agent", "Firefox")],
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        let rows = audit_of(&state, team.id).await;
        let first: Vec<&Value> = rows
            .iter()
            .filter(|e| e["action"] == "auth.login")
            .collect();
        assert_eq!(first.len(), 1, "{rows:?}");
        assert_eq!(first[0]["actor"]["id"], json!(member));
        assert_eq!(first[0]["data"]["via"], "session_first_use");
        assert_eq!(
            first[0]["data"]["session_id"],
            home[0]["data"]["session_id"]
        );

        // A guest's requests in the host tenant write nothing.
        for _ in 0..2 {
            let r = call(
                &app,
                Method::GET,
                "/v1/me/app-passwords",
                &[("cookie", &guest_cookie), ("ruum-tenant", &team_id)],
                None,
            )
            .await;
            assert_eq!(r.status, StatusCode::OK);
        }
        assert_eq!(logins(&audit_of(&state, team.id).await), 1);
        // The owner's own team: also a first use there (the home tenant is the personal one).
        let r = call(
            &app,
            Method::GET,
            "/v1/me/app-passwords",
            &[("cookie", &owner_cookie), ("ruum-tenant", &team_id)],
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(logins(&audit_of(&state, team.id).await), 2);

        // Sign-out writes auth.logout in the home tenant; a new session counts again.
        let out = call(
            &app,
            Method::POST,
            "/v1/auth/logout",
            &[("cookie", &member_cookie), ("ruum-csrf", &member_csrf)],
            None,
        )
        .await;
        assert_eq!(out.status, StatusCode::OK);
        let home = audit_of(&state, member_home).await;
        assert_eq!(home.last().unwrap()["action"], "auth.logout");
        let (again, _, _, _) = signed_in(&app, &oidc, "member@example.test").await;
        let r = call(
            &app,
            Method::GET,
            "/v1/me/app-passwords",
            &[("cookie", &again), ("ruum-tenant", &team_id)],
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(logins(&audit_of(&state, team.id).await), 3);
    }

    /// An audit log whose writes of one action fail.
    struct FailingAudit {
        inner: Arc<dyn ruum_core::audit::AuditLog>,
        action: &'static str,
    }

    #[async_trait::async_trait]
    impl ruum_core::audit::AuditLog for FailingAudit {
        async fn record(
            &self,
            ctx: &ruum_core::Context,
            entry: ruum_core::audit::NewAuditEntry,
        ) -> ruum_core::error::Result<()> {
            if entry.action == self.action {
                return Err(ruum_core::Error::Internal("the audit log is down".into()));
            }
            self.inner.record(ctx, entry).await
        }
        async fn record_first_use(
            &self,
            ctx: &ruum_core::Context,
            session: uuid::Uuid,
            entry: ruum_core::audit::NewAuditEntry,
        ) -> ruum_core::error::Result<bool> {
            if entry.action == self.action {
                return Err(ruum_core::Error::Internal("the audit log is down".into()));
            }
            self.inner.record_first_use(ctx, session, entry).await
        }
        async fn list(
            &self,
            ctx: &ruum_core::Context,
            query: ruum_core::audit::AuditQuery,
        ) -> ruum_core::error::Result<ruum_core::Page<ruum_core::audit::AuditEntry>> {
            self.inner.list(ctx, query).await
        }
    }

    /// A failed `auth.login` write fails the sign-in: `500`, no session cookie, and the new
    /// session is ended (design 15, decision 0003-4).
    #[tokio::test]
    async fn a_failed_sign_in_audit_fails_the_sign_in() {
        let (_, oidc, mut state) = app();
        state.services.audit = Arc::new(FailingAudit {
            inner: state.services.audit.clone(),
            action: "auth.login",
        });
        let app = router(state.clone());
        let start = call(&app, Method::GET, "/v1/auth/login", &[], None).await;
        let location = start.headers["location"].to_str().unwrap().to_string();
        let st = url::Url::parse(&location)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .into_owned();
        let login_cookie = cookie_value(&start, "ruum_login");
        oidc.add_login(
            "code-9",
            VerifiedLogin {
                issuer: MemoryOidcClient::ISSUER.into(),
                subject: "down@example.test".into(),
                email: Some("down@example.test".into()),
                email_verified: true,
                name: None,
                id_token: None,
            },
        );
        let cb = call(
            &app,
            Method::GET,
            &format!("/v1/auth/callback?code=code-9&state={st}"),
            &[("cookie", &format!("ruum_login={login_cookie}"))],
            None,
        )
        .await;
        assert_eq!(cb.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(cookie_value(&cb, "ruum_session"), "");
        let user = state
            .services
            .identity
            .find_verified_email("down@example.test")
            .await
            .unwrap()
            .unwrap();
        assert!(
            state
                .services
                .identity
                .sessions_of(user.id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A first use whose audit write fails fails the request (design 15).
    #[tokio::test]
    async fn a_failed_first_use_audit_fails_the_request() {
        let (_, oidc, mut state) = app();
        let audit = state.services.audit.clone();
        let app = router(state.clone());
        let (_, _, owner, _) = signed_in(&app, &oidc, "boss@example.test").await;
        let team = state
            .services
            .tenants
            .create_team(&ruum_core::Actor::user(owner), "Team".into())
            .await
            .unwrap();
        let (cookie, _, _, _) = signed_in(&app, &oidc, "boss@example.test").await;
        state.services.audit = Arc::new(FailingAudit {
            inner: audit,
            action: "auth.login",
        });
        let app = router(state);
        let r = call(
            &app,
            Method::GET,
            "/v1/me/app-passwords",
            &[("cookie", &cookie), ("ruum-tenant", &team.id.to_string())],
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
