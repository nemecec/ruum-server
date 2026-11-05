//! Routes of the `agents` area. See `docs/STEP1.md` for the owner of this file.

use crate::error::{HttpError, not_implemented};
use crate::state::AppState;
use axum::Router;
use axum::routing::{delete, get, post};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/agents", get(list_agents).post(register_agent))
        .route("/v1/agents/{agent_id}", get(get_agent).patch(update_agent))
        .route("/v1/agents/{agent_id}/disable", post(disable_agent))
        .route("/v1/agents/{agent_id}/enable", post(enable_agent))
        .route(
            "/v1/agents/{agent_id}/keys",
            get(list_agent_keys).post(create_agent_key),
        )
        .route(
            "/v1/agents/{agent_id}/keys/{key_id}",
            delete(revoke_agent_key),
        )
        .route("/v1/agents/{agent_id}/activity", get(list_agent_activity))
        .route("/v1/agents/{agent_id}/undo", post(undo_agent))
        .route("/v1/oauth/token", post(exchange_token))
}

/// List agents. `GET /v1/agents` (operationId `listAgents`).
pub async fn list_agents() -> HttpError {
    not_implemented("listAgents")
}

/// Register an agent. `POST /v1/agents` (operationId `registerAgent`).
pub async fn register_agent() -> HttpError {
    not_implemented("registerAgent")
}

/// Get an agent. `GET /v1/agents/{agent_id}` (operationId `getAgent`).
pub async fn get_agent() -> HttpError {
    not_implemented("getAgent")
}

/// Change an agent's name or scope. `PATCH /v1/agents/{agent_id}` (operationId `updateAgent`).
pub async fn update_agent() -> HttpError {
    not_implemented("updateAgent")
}

/// Disable an agent. `POST /v1/agents/{agent_id}/disable` (operationId `disableAgent`).
pub async fn disable_agent() -> HttpError {
    not_implemented("disableAgent")
}

/// Enable an agent. `POST /v1/agents/{agent_id}/enable` (operationId `enableAgent`).
pub async fn enable_agent() -> HttpError {
    not_implemented("enableAgent")
}

/// List the API keys of an agent. `GET /v1/agents/{agent_id}/keys` (operationId `listAgentKeys`).
pub async fn list_agent_keys() -> HttpError {
    not_implemented("listAgentKeys")
}

/// Create an API key. `POST /v1/agents/{agent_id}/keys` (operationId `createAgentKey`).
pub async fn create_agent_key() -> HttpError {
    not_implemented("createAgentKey")
}

/// Revoke an API key. `DELETE /v1/agents/{agent_id}/keys/{key_id}` (operationId `revokeAgentKey`).
pub async fn revoke_agent_key() -> HttpError {
    not_implemented("revokeAgentKey")
}

/// Changes made by an agent. `GET /v1/agents/{agent_id}/activity` (operationId `listAgentActivity`).
pub async fn list_agent_activity() -> HttpError {
    not_implemented("listAgentActivity")
}

/// Undo what an agent did since a time. `POST /v1/agents/{agent_id}/undo` (operationId `undoAgent`).
pub async fn undo_agent() -> HttpError {
    not_implemented("undoAgent")
}

/// Exchange an API key for an access token. `POST /v1/oauth/token` (operationId `exchangeToken`).
pub async fn exchange_token() -> HttpError {
    not_implemented("exchangeToken")
}
