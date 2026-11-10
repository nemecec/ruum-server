//! Errors of the engines.
//!
//! The texts here can hold engine text (`Sql::message`). The query layer sanitizes them for
//! agents and for people without `manage` (decision 0004-8); the engine never adds row values.

/// An error of a [`crate::DatabaseEngine`] call.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// No database with this reference (also: the tenant or node does not match).
    #[error("database not found")]
    NotFound,
    /// No restore point at this mark (or outside what the engine keeps).
    #[error("restore point not found")]
    RestorePointNotFound,
    /// The role reference is not valid (for example the kind `mask`).
    #[error("invalid role reference: {0}")]
    InvalidRole(String),
    /// A statement that changes data or the schema in a read-only request or role.
    #[error("a read-only query cannot change data")]
    ReadOnly,
    /// The statement ran longer than its timeout and was cancelled.
    #[error("statement timeout")]
    Timeout,
    /// The request held more than one statement.
    #[error("only one statement is allowed")]
    MultipleStatements,
    /// The engine refused the statement before it ran (transaction control, `ATTACH`,
    /// a `PRAGMA` outside the allow-list, a reserved name). The text names the rule.
    #[error("statement not allowed: {0}")]
    StatementNotAllowed(String),
    /// An untrusted statement ended the request's transaction (decision 0004-34 d). The engine
    /// failed the request and closed (or reset) the connection.
    #[error("the statement ended the transaction")]
    TransactionEnded,
    /// The role of the request may not do this (for example DDL as `write`).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// An error of the engine for the statement: a code (SQLSTATE on Postgres, the extended
    /// result code name on SQLite) and the engine's text, not sanitized yet.
    ///
    /// `in_routine`: the error came from inside a function, a trigger or a `RAISE` (Postgres
    /// reports a `CONTEXT`; SQLite reports `SQLITE_CONSTRAINT_TRIGGER`). Tenant code may have
    /// written its text, its SQLSTATE and its fields, with the values of the real row, so no
    /// part of it is shown to a caller without `manage` (review-Q-2-2 P-2).
    #[error("sql error {code}: {message}")]
    Sql {
        code: String,
        message: String,
        in_routine: bool,
    },
    /// A migration failed and was rolled back; the catalog sync and the lens rebuild still
    /// ran on the unchanged schema (decision 0004-34 f).
    #[error("migration failed: {0}")]
    MigrationFailed(Box<EngineError>),
    /// The caller's catalog sync failed.
    #[error("catalog sync failed: {0}")]
    CatalogSync(String),
    /// The engine does not support this (yet). The text says what and which card adds it.
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The database is paused (a copy drains it, a migration rebuilds its lens); retry.
    #[error("database not ready")]
    NotReady,
    /// A failure of the engine itself.
    #[error("engine error: {0}")]
    Internal(String),
}

impl EngineError {
    /// Whether the caller may retry the same request later (`database_not_ready`).
    pub fn is_retryable(&self) -> bool {
        matches!(self, EngineError::NotReady)
    }
}
