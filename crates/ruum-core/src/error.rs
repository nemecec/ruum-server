//! Errors. The API sends them as
//! `{ "error": { "code": "snake_case_code", "message": "...", "hint": "..." } }`.
//! Codes are stable and machine-readable; the hint tells an agent the next step (section 11).

use serde::{Deserialize, Serialize};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// The stable error codes. The list is part of the API contract (`contracts/openapi.yaml`,
/// schema `ErrorCode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    ValidationFailed,
    Unauthenticated,
    TokenExpired,
    PermissionDenied,
    ScopeExceeded,
    NotFound,
    Conflict,
    NameTaken,
    VersionMismatch,
    IdempotencyKeyReused,
    PreconditionFailed,
    PayloadTooLarge,
    RangeNotSatisfiable,
    QuotaExceeded,
    PlanLimitReached,
    RateLimited,
    AgentDisabled,
    ChunkMissing,
    HashMismatch,
    NotImplemented,
    Internal,
    Unavailable,
}

impl ErrorCode {
    /// The HTTP status for this code.
    pub fn http_status(self) -> u16 {
        use ErrorCode::*;
        match self {
            BadRequest => 400,
            ValidationFailed => 422,
            Unauthenticated | TokenExpired => 401,
            PermissionDenied | ScopeExceeded | AgentDisabled => 403,
            NotFound => 404,
            Conflict | NameTaken | VersionMismatch | IdempotencyKeyReused | ChunkMissing => 409,
            PreconditionFailed => 412,
            PayloadTooLarge => 413,
            RangeNotSatisfiable => 416,
            HashMismatch => 422,
            QuotaExceeded | PlanLimitReached => 403,
            RateLimited => 429,
            NotImplemented => 501,
            Internal => 500,
            Unavailable => 503,
        }
    }

    /// The code as the snake_case string of the wire format.
    pub fn as_str(self) -> &'static str {
        use ErrorCode::*;
        match self {
            BadRequest => "bad_request",
            ValidationFailed => "validation_failed",
            Unauthenticated => "unauthenticated",
            TokenExpired => "token_expired",
            PermissionDenied => "permission_denied",
            ScopeExceeded => "scope_exceeded",
            NotFound => "not_found",
            Conflict => "conflict",
            NameTaken => "name_taken",
            VersionMismatch => "version_mismatch",
            IdempotencyKeyReused => "idempotency_key_reused",
            PreconditionFailed => "precondition_failed",
            PayloadTooLarge => "payload_too_large",
            RangeNotSatisfiable => "range_not_satisfiable",
            QuotaExceeded => "quota_exceeded",
            PlanLimitReached => "plan_limit_reached",
            RateLimited => "rate_limited",
            AgentDisabled => "agent_disabled",
            ChunkMissing => "chunk_missing",
            HashMismatch => "hash_mismatch",
            NotImplemented => "not_implemented",
            Internal => "internal",
            Unavailable => "unavailable",
        }
    }
}

/// The error body of the API (the value of the `error` key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

/// The error type of the core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An error that the API shows to the client as it is.
    #[error("{}: {}", .0.code.as_str(), .0.message)]
    Api(ApiError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("object store error: {0}")]
    ObjectStore(#[from] object_store::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn api(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Api(ApiError::new(code, message))
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        Self::api(ErrorCode::NotFound, format!("{} not found", what.into()))
    }

    pub fn permission_denied() -> Self {
        Self::api(
            ErrorCode::PermissionDenied,
            "you do not have the permission for this action",
        )
    }

    pub fn not_implemented(what: impl Into<String>) -> Self {
        Self::Api(
            ApiError::new(
                ErrorCode::NotImplemented,
                format!("{} is not implemented yet", what.into()),
            )
            .with_hint("this endpoint is part of step 1 and is still being built"),
        )
    }

    /// The error as the client sees it. Internal details are not shown.
    pub fn to_api(&self) -> ApiError {
        match self {
            Error::Api(e) => e.clone(),
            Error::Database(_) | Error::ObjectStore(_) | Error::Internal(_) => {
                ApiError::new(ErrorCode::Internal, "an internal error occurred")
                    .with_hint("retry later; if it happens again, report the request ID")
            }
        }
    }

    pub fn http_status(&self) -> u16 {
        self.to_api().code.http_status()
    }
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        Error::Api(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_serializes_as_snake_case() {
        let s = serde_json::to_string(&ErrorCode::NameTaken).unwrap();
        assert_eq!(s, "\"name_taken\"");
        assert_eq!(ErrorCode::NameTaken.as_str(), "name_taken");
    }

    #[test]
    fn internal_errors_are_hidden() {
        let e = Error::Internal("secret detail".into());
        assert_eq!(e.to_api().code, ErrorCode::Internal);
        assert!(!e.to_api().message.contains("secret"));
    }
}
