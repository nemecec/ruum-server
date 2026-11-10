//! Errors of the Postgres engine.
//!
//! The text of an engine error is sanitized here already (decision 0004-8): `EngineError::Sql`
//! holds the SQLSTATE and the primary message only, never `DETAIL`, `HINT` or `CONTEXT`, which
//! carry row values (`Key (email)=(...)`, `Failing row contains (...)`). The query layer
//! replaces the text with a fixed one per SQLSTATE for agents and people without `manage`, and
//! shows no object name of an error that came from inside a function or a trigger
//! (`in_routine`).

use crate::error::EngineError;

/// Why a platform statement failed (never shown to a caller with the SQL).
pub(crate) fn internal(context: &str, e: sqlx::Error) -> EngineError {
    match &e {
        sqlx::Error::Database(db) => EngineError::Internal(format!(
            "{context}: {} ({})",
            db.message(),
            db.code().unwrap_or_default()
        )),
        _ => EngineError::Internal(format!("{context}: {e}")),
    }
}

/// The SQLSTATE of an error, if it is a database error.
pub(crate) fn sqlstate(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(db) => db.code().map(|c| c.into_owned()),
        _ => None,
    }
}

/// Whether the connection broke (the server ended it, the socket closed).
pub(crate) fn is_connection_lost(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Io(_) | sqlx::Error::Protocol(_) | sqlx::Error::Tls(_) => true,
        sqlx::Error::Database(db) => matches!(
            db.code().as_deref(),
            Some("57P01" | "57P02" | "57P03" | "08000" | "08003" | "08006")
        ),
        _ => false,
    }
}

/// Maps the error of an untrusted statement (a query or a statement of a migration's `up`).
pub(crate) fn untrusted(e: sqlx::Error) -> EngineError {
    if is_connection_lost(&e) {
        // The drain of a TEMPLATE copy ended the session (decision 0004-27): retry.
        return EngineError::NotReady;
    }
    let sqlx::Error::Database(db) = &e else {
        return EngineError::Internal(e.to_string());
    };
    let code = db.code().map(|c| c.into_owned()).unwrap_or_default();
    let message = db.message().to_string();
    // A `CONTEXT` means the error came from inside a function or a trigger: tenant code may have
    // chosen its text, SQLSTATE and fields (review-Q-2-2 P-2).
    let in_routine = db
        .try_downcast_ref::<sqlx::postgres::PgDatabaseError>()
        .is_some_and(|pg| pg.r#where().is_some());
    match code.as_str() {
        "57014" if message.contains("statement timeout") => EngineError::Timeout,
        // Cancelled by the drain of a copy ("canceling statement due to user request").
        "57014" => EngineError::NotReady,
        "25006" => EngineError::ReadOnly,
        "42501" => EngineError::PermissionDenied(message),
        "42601" if message.contains("cannot insert multiple commands") => {
            EngineError::MultipleStatements
        }
        _ => EngineError::Sql {
            code,
            message,
            in_routine,
        },
    }
}
