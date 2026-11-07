//! The per-connection guard of the memory engine: the authorizer and the timeout.
//!
//! Platform statements (setup, catalog, dumps) run with the guard in `Trusted` mode. Untrusted
//! statements (queries and a migration's `up`) run in `Untrusted` mode, where the authorizer
//! refuses, at prepare time and so before anything runs:
//!
//! - transaction control (`BEGIN`, `COMMIT`, `END`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`), the
//!   engine line of decision 0004-34 (d);
//! - `ATTACH`, `DETACH`, a `PRAGMA` outside the read-only allow-list, `load_extension`,
//!   `readfile`, `writefile`, `ruum_pseudonym`, TEMP objects (decision 0004-2);
//! - DDL for a kind other than `owner`;
//! - every object named `ruum_*` (the Ruum objects of the database).
//!
//! The SQLite engine of card E-3 builds its own, fuller hardening; this is the fake's.

use crate::lock::Mutex;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Instant;

const TRUSTED: u8 = 0;
const UNTRUSTED_OWNER: u8 = 1;
const UNTRUSTED: u8 = 2;

/// The PRAGMAs an untrusted statement may run (they read the schema only).
const PRAGMA_ALLOW: &[&str] = &[
    "table_info",
    "table_xinfo",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
];

/// Functions an untrusted statement may never call.
const FUNCTION_DENY: &[&str] = &["load_extension", "readfile", "writefile", "ruum_pseudonym"];

/// Why the authorizer refused a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denial {
    TransactionControl,
    Attach,
    Pragma,
    Function,
    Temp,
    Ddl,
    ReservedName,
}

impl Denial {
    pub(crate) fn rule(self) -> &'static str {
        match self {
            Denial::TransactionControl => "transaction_control",
            Denial::Attach => "attach",
            Denial::Pragma => "pragma",
            Denial::Function => "function",
            Denial::Temp => "temporary_object",
            Denial::Ddl => "schema_change",
            Denial::ReservedName => "reserved_name",
        }
    }
}

#[derive(Default)]
pub(crate) struct Guard {
    mode: AtomicU8,
    denied: Mutex<Option<Denial>>,
    deadline: Mutex<Option<Instant>>,
    timed_out: AtomicBool,
    /// Whether the authorizer allowed DDL in the current untrusted statement.
    changed_schema: AtomicBool,
    /// Tests only: let transaction control through the authorizer, to show that the check after
    /// each statement (the second line) catches it.
    allow_transaction_control: AtomicBool,
}

impl Guard {
    pub(crate) fn new() -> Arc<Guard> {
        Arc::new(Guard::default())
    }

    /// Enters untrusted mode for a statement of a kind with (`owner`) or without schema rights.
    pub(crate) fn enter_untrusted(&self, schema_rights: bool) {
        *self.denied.lock() = None;
        self.changed_schema.store(false, Ordering::SeqCst);
        let mode = if schema_rights {
            UNTRUSTED_OWNER
        } else {
            UNTRUSTED
        };
        self.mode.store(mode, Ordering::SeqCst);
    }

    pub(crate) fn leave_untrusted(&self) {
        self.mode.store(TRUSTED, Ordering::SeqCst);
    }

    /// Whether the latest untrusted statement changed the schema (DDL that the authorizer let
    /// through).
    pub(crate) fn changed_schema(&self) -> bool {
        self.changed_schema.load(Ordering::SeqCst)
    }

    pub(crate) fn take_denial(&self) -> Option<Denial> {
        self.denied.lock().take()
    }

    pub(crate) fn start_timer(&self, deadline: Instant) {
        self.timed_out.store(false, Ordering::SeqCst);
        *self.deadline.lock() = Some(deadline);
    }

    pub(crate) fn stop_timer(&self) {
        *self.deadline.lock() = None;
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn set_allow_transaction_control(&self, allow: bool) {
        self.allow_transaction_control
            .store(allow, Ordering::SeqCst);
    }

    /// The progress handler: `true` interrupts the running statement.
    pub(crate) fn progress(&self) -> bool {
        match *self.deadline.lock() {
            Some(deadline) if Instant::now() >= deadline => {
                self.timed_out.store(true, Ordering::SeqCst);
                true
            }
            _ => false,
        }
    }

    /// The authorizer.
    pub(crate) fn authorize(&self, ctx: AuthContext<'_>) -> Authorization {
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == TRUSTED {
            return Authorization::Allow;
        }
        match self.check(&ctx.action, mode == UNTRUSTED_OWNER) {
            None => Authorization::Allow,
            Some(denial) => {
                let mut slot = self.denied.lock();
                if slot.is_none() {
                    *slot = Some(denial);
                }
                Authorization::Deny
            }
        }
    }

    /// The rule for DDL; records DDL that it allows.
    fn ddl(&self, schema_rights: bool, reserved: bool) -> Option<Denial> {
        if !schema_rights {
            Some(Denial::Ddl)
        } else if reserved {
            Some(Denial::ReservedName)
        } else {
            self.changed_schema.store(true, Ordering::SeqCst);
            None
        }
    }

    fn check(&self, action: &AuthAction<'_>, schema_rights: bool) -> Option<Denial> {
        use AuthAction as A;
        let reserved = |name: &str| name.to_ascii_lowercase().starts_with("ruum_");
        match action {
            A::Transaction { .. } | A::Savepoint { .. } => {
                if self.allow_transaction_control.load(Ordering::SeqCst) {
                    None
                } else {
                    Some(Denial::TransactionControl)
                }
            }
            A::Attach { .. } | A::Detach { .. } => Some(Denial::Attach),
            A::Pragma { pragma_name, .. } => {
                let name = pragma_name.to_ascii_lowercase();
                (!PRAGMA_ALLOW.contains(&name.as_str())).then_some(Denial::Pragma)
            }
            A::Function { function_name } => {
                let name = function_name.to_ascii_lowercase();
                FUNCTION_DENY
                    .contains(&name.as_str())
                    .then_some(Denial::Function)
            }
            A::CreateTempIndex { .. }
            | A::CreateTempTable { .. }
            | A::CreateTempTrigger { .. }
            | A::CreateTempView { .. }
            | A::DropTempIndex { .. }
            | A::DropTempTable { .. }
            | A::DropTempTrigger { .. }
            | A::DropTempView { .. } => Some(Denial::Temp),
            A::CreateTable { table_name }
            | A::DropTable { table_name }
            | A::AlterTable { table_name, .. }
            | A::Analyze { table_name }
            | A::CreateVtable { table_name, .. }
            | A::DropVtable { table_name, .. } => self.ddl(schema_rights, reserved(table_name)),
            A::CreateIndex {
                index_name,
                table_name,
            }
            | A::DropIndex {
                index_name,
                table_name,
            } => self.ddl(schema_rights, reserved(index_name) || reserved(table_name)),
            A::CreateTrigger {
                trigger_name,
                table_name,
            }
            | A::DropTrigger {
                trigger_name,
                table_name,
            } => self.ddl(
                schema_rights,
                reserved(trigger_name) || reserved(table_name),
            ),
            A::CreateView { view_name } | A::DropView { view_name } => {
                self.ddl(schema_rights, reserved(view_name))
            }
            A::Reindex { index_name } => self.ddl(schema_rights, reserved(index_name)),
            A::Read { table_name, .. }
            | A::Insert { table_name }
            | A::Update { table_name, .. }
            | A::Delete { table_name } => reserved(table_name).then_some(Denial::ReservedName),
            _ => None,
        }
    }
}
