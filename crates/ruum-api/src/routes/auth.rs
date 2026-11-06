//! Routes of the `auth` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get, post};

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

/// Start OIDC sign-in. `GET /v1/auth/login` (operationId `loginStart`).
pub async fn login_start() -> HttpError {
    not_implemented("loginStart")
}

/// OIDC callback. `GET /v1/auth/callback` (operationId `loginCallback`).
pub async fn login_callback() -> HttpError {
    not_implemented("loginCallback")
}

/// Sign out. `POST /v1/auth/logout` (operationId `logout`).
pub async fn logout() -> HttpError {
    not_implemented("logout")
}

/// The signed-in principal. `GET /v1/me` (operationId `getMe`).
pub async fn get_me() -> HttpError {
    not_implemented("getMe")
}

/// Linked identities of the signed-in user. `GET /v1/me/identities` (operationId `listMyIdentities`).
pub async fn list_my_identities() -> HttpError {
    not_implemented("listMyIdentities")
}

/// Web sessions of the signed-in user. `GET /v1/me/sessions` (operationId `listMySessions`).
pub async fn list_my_sessions() -> HttpError {
    not_implemented("listMySessions")
}

/// End one web session. `DELETE /v1/me/sessions/{session_id}` (operationId `endSession`).
pub async fn end_session() -> HttpError {
    not_implemented("endSession")
}

/// App passwords of the signed-in user in one tenant. `GET /v1/me/app-passwords` (operationId
/// `listAppPasswords`).
pub async fn list_app_passwords() -> HttpError {
    not_implemented("listAppPasswords")
}

/// Create a WebDAV app password. `POST /v1/me/app-passwords` (operationId `createAppPassword`).
pub async fn create_app_password() -> HttpError {
    not_implemented("createAppPassword")
}

/// Revoke an app password. `DELETE /v1/me/app-passwords/{app_password_id}` (operationId
/// `revokeAppPassword`).
pub async fn revoke_app_password() -> HttpError {
    not_implemented("revokeAppPassword")
}
