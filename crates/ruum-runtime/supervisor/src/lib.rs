//! The sandbox supervisor (backlog I6-20, S-89; `docs/STEP3.md` "The sandbox supervisor",
//! decision 0006 I).
//!
//! `ruum-supervisor` is the first process of every app sandbox. It reads the run's launch data
//! (the app's command, its secret environment values, the values to redact, the workload token),
//! starts the app as its child, and redacts the app's standard output and standard error before
//! they reach the container runtime: the app never holds the container's own output streams, so
//! the node's log files (`/var/log/pods` on Kubernetes) hold only redacted lines.
//!
//! - [`protocol`]: the launch data and the token answer, shared with the drivers
//!   (`crates/ruum-runtime`), and their limits.
//! - [`run`]: the supervisor itself (spawn, redacting pumps, signals, reaping, exit code).
//! - [`http`]: the small HTTP/1.1 client of the Kubernetes launch channel (no TLS; the design's
//!   transport section says why and what comes before real tenants).
//! - [`redact`]: the redactor of `ruum-core` (`runtime::redact`), compiled here from the same
//!   file, so the sandbox and the driver redact with the same code. It is not a copy.
//!
//! The crate has no `unsafe` code: `nix` wraps the process calls of a PID 1.

pub mod http;
pub mod protocol;
pub mod run;

/// The redactor of `ruum-core`, from its own source file (see the module docs). Its two checks
/// return this crate's [`RuntimeError`]. Its crate-private helpers serve `ruum-core`'s own tests.
#[allow(rustdoc::broken_intra_doc_links, dead_code)]
#[path = "../../../ruum-core/src/runtime/redact.rs"]
pub mod redact;

/// The error type that [`redact::check_secret_set`] and [`redact::check_secret_value`] return
/// here (in `ruum-core` they return its `RuntimeError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    pub field: String,
    pub reason: String,
}

impl RuntimeError {
    pub fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.reason)
    }
}

impl std::error::Error for RuntimeError {}
