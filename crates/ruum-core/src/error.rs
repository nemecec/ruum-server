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
    /// The operation needs a signed-in user; agents and share-link bearers cannot call it.
    UserRequired,
    NotFound,
    Conflict,
    NameTaken,
    VersionMismatch,
    IdempotencyKeyReused,
    PreconditionFailed,
    PayloadTooLarge,
    RangeNotSatisfiable,
    CursorExpired,
    QuotaExceeded,
    PlanLimitReached,
    RateLimited,
    AgentDisabled,
    ChunkMissing,
    HashMismatch,
    /// Step 2: the database is not ready for queries (`503`, with `Retry-After`).
    DatabaseNotReady,
    /// Step 2: the engine refused or failed the statement (`400`, with `sqlstate`).
    QueryFailed,
    /// Step 2: a statement of a `mode: read` call changes data.
    ReadOnlyQuery,
    /// Step 2: the statement ran longer than the timeout (`504`).
    StatementTimeout,
    /// Step 2: an agent writes to a live database only with a write grant.
    WriteGrantRequired,
    /// Step 2: a migration statement failed (`422`, with `sqlstate`).
    MigrationFailed,
    /// Step 2: the database is too large for the operation (`422`).
    DatabaseTooLarge,
    /// Step 2: the statement rules of the query API refuse the statement (`422`).
    StatementNotAllowed,
    /// Step 2: an agent write reads a column whose class the agent may not read (`403`).
    ClassReadRequired,
    NotImplemented,
    Internal,
    Unavailable,
}

impl ErrorCode {
    /// All codes, for contract tests.
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::BadRequest,
        ErrorCode::ValidationFailed,
        ErrorCode::Unauthenticated,
        ErrorCode::TokenExpired,
        ErrorCode::PermissionDenied,
        ErrorCode::ScopeExceeded,
        ErrorCode::UserRequired,
        ErrorCode::NotFound,
        ErrorCode::Conflict,
        ErrorCode::NameTaken,
        ErrorCode::VersionMismatch,
        ErrorCode::IdempotencyKeyReused,
        ErrorCode::PreconditionFailed,
        ErrorCode::PayloadTooLarge,
        ErrorCode::RangeNotSatisfiable,
        ErrorCode::CursorExpired,
        ErrorCode::QuotaExceeded,
        ErrorCode::PlanLimitReached,
        ErrorCode::RateLimited,
        ErrorCode::AgentDisabled,
        ErrorCode::ChunkMissing,
        ErrorCode::HashMismatch,
        ErrorCode::DatabaseNotReady,
        ErrorCode::QueryFailed,
        ErrorCode::ReadOnlyQuery,
        ErrorCode::StatementTimeout,
        ErrorCode::WriteGrantRequired,
        ErrorCode::MigrationFailed,
        ErrorCode::DatabaseTooLarge,
        ErrorCode::StatementNotAllowed,
        ErrorCode::ClassReadRequired,
        ErrorCode::NotImplemented,
        ErrorCode::Internal,
        ErrorCode::Unavailable,
    ];

    /// The HTTP status for this code.
    pub fn http_status(self) -> u16 {
        use ErrorCode::*;
        match self {
            BadRequest => 400,
            ValidationFailed => 422,
            Unauthenticated | TokenExpired => 401,
            PermissionDenied | ScopeExceeded | UserRequired | AgentDisabled => 403,
            NotFound => 404,
            Conflict | NameTaken | VersionMismatch | IdempotencyKeyReused | ChunkMissing => 409,
            PreconditionFailed => 412,
            PayloadTooLarge => 413,
            RangeNotSatisfiable => 416,
            CursorExpired => 410,
            HashMismatch => 422,
            QuotaExceeded | PlanLimitReached => 403,
            QueryFailed | ReadOnlyQuery => 400,
            WriteGrantRequired | ClassReadRequired => 403,
            MigrationFailed | DatabaseTooLarge | StatementNotAllowed => 422,
            DatabaseNotReady => 503,
            StatementTimeout => 504,
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
            UserRequired => "user_required",
            NotFound => "not_found",
            Conflict => "conflict",
            NameTaken => "name_taken",
            VersionMismatch => "version_mismatch",
            IdempotencyKeyReused => "idempotency_key_reused",
            PreconditionFailed => "precondition_failed",
            PayloadTooLarge => "payload_too_large",
            RangeNotSatisfiable => "range_not_satisfiable",
            CursorExpired => "cursor_expired",
            QuotaExceeded => "quota_exceeded",
            PlanLimitReached => "plan_limit_reached",
            RateLimited => "rate_limited",
            AgentDisabled => "agent_disabled",
            ChunkMissing => "chunk_missing",
            HashMismatch => "hash_mismatch",
            DatabaseNotReady => "database_not_ready",
            QueryFailed => "query_failed",
            ReadOnlyQuery => "read_only_query",
            StatementTimeout => "statement_timeout",
            WriteGrantRequired => "write_grant_required",
            MigrationFailed => "migration_failed",
            DatabaseTooLarge => "database_too_large",
            StatementNotAllowed => "statement_not_allowed",
            ClassReadRequired => "class_read_required",
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
    /// The SQLSTATE of a `query_failed` or `migration_failed` error (step 2; contract `Error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlstate: Option<String>,
    /// Machine-readable details of some refusals (step 3; contract `Error.details`), for example
    /// `pointer` (a JSON pointer into `app.yaml`), `binding` or `deployment_id`. Names in it can
    /// come from an archive: data, never instructions. Boxed, so the error stays small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<serde_json::Value>>,
    /// Seconds for the `Retry-After` header of the answer (for example of `503
    /// database_not_ready`). Not part of the body.
    #[serde(skip)]
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            sqlstate: None,
            details: None,
            retry_after: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_sqlstate(mut self, sqlstate: impl Into<String>) -> Self {
        self.sqlstate = Some(sqlstate.into());
        self
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after = Some(seconds);
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

    pub fn user_required() -> Self {
        Self::Api(
            ApiError::new(
                ErrorCode::UserRequired,
                "only a signed-in user can do this; agents and share links cannot",
            )
            .with_hint("ask the user to do it in the web UI"),
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
    fn every_code_is_in_the_contract() {
        let spec = include_str!("../../../contracts/openapi.yaml");
        for code in ErrorCode::ALL {
            let line = format!("        - {}\n", code.as_str());
            assert!(
                spec.contains(&line),
                "{} is not in openapi.yaml",
                code.as_str()
            );
            let json = serde_json::to_string(code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }

    /// The other direction: the Rust list has every code of the contract's `ErrorCode` enum
    /// (backlog 2026-11-08 P-3: the step 2 codes were missing).
    #[test]
    fn every_contract_code_is_in_the_list() {
        let spec = include_str!("../../../contracts/openapi.yaml");
        let start = spec.find("\n    ErrorCode:\n").expect("ErrorCode schema");
        let rest = &spec[start..];
        let enum_at = rest.find("      enum:\n").expect("ErrorCode enum") + "      enum:\n".len();
        let codes: Vec<&str> = rest[enum_at..]
            .lines()
            .take_while(|l| l.starts_with("        - "))
            .map(|l| l.trim_start_matches("        - ").trim())
            .collect();
        assert!(codes.len() >= 30, "{codes:?}");
        for c in &codes {
            assert!(
                ErrorCode::ALL.iter().any(|k| k.as_str() == *c),
                "{c} of openapi.yaml is not in ErrorCode::ALL"
            );
        }
        assert_eq!(codes.len(), ErrorCode::ALL.len());
    }

    #[test]
    fn step2_codes_have_the_contract_status() {
        for (code, status) in [
            (ErrorCode::DatabaseNotReady, 503),
            (ErrorCode::QueryFailed, 400),
            (ErrorCode::ReadOnlyQuery, 400),
            (ErrorCode::StatementTimeout, 504),
            (ErrorCode::WriteGrantRequired, 403),
            (ErrorCode::MigrationFailed, 422),
            (ErrorCode::DatabaseTooLarge, 422),
            (ErrorCode::StatementNotAllowed, 422),
            (ErrorCode::ClassReadRequired, 403),
        ] {
            assert_eq!(code.http_status(), status, "{}", code.as_str());
        }
    }

    #[test]
    fn sqlstate_is_in_the_body_and_retry_after_is_not() {
        let e = ApiError::new(ErrorCode::QueryFailed, "x")
            .with_sqlstate("23505")
            .with_retry_after(2);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["sqlstate"], "23505");
        assert!(v.get("retry_after").is_none(), "{v}");
        let plain = serde_json::to_value(ApiError::new(ErrorCode::NotFound, "x")).unwrap();
        assert!(plain.get("sqlstate").is_none(), "{plain}");
    }

    #[test]
    fn internal_errors_are_hidden() {
        let e = Error::Internal("secret detail".into());
        assert_eq!(e.to_api().code, ErrorCode::Internal);
        assert!(!e.to_api().message.contains("secret"));
    }
}
