//! The privacy suite (design 8.5, decisions 0004-3, 0004-4, 0004-5, 0004-8, 0004-9, 0004-24,
//! 0004-25 and 0004-29; card T-2).
//!
//! The rule: an agent never sees a raw `secret` value, nor a raw `personal` value without a
//! `read_personal` grant, through any operation, any clause of a statement or any side channel.
//! People without `manage` never see a raw `secret` value. Raw values are never stored in the
//! query log, in approvals or in a status message.
//!
//! # How a case works
//!
//! Each [`Case`] names the contract operations it needs, the engines it runs on and what the
//! memory backend does with it. A case runs once per engine, in a world of its own
//! ([`Backend::world`]): it changes classes and grants, so it never shares state.
//!
//! The case fixture ([`Fixture`]) is built through the API on top of the `fixtures.rs` world:
//! `member` (a person with `manage` on `Private`) creates a database in `Private`, applies one
//! migration with two tables (`people`, `orders`), confirms the classes of every column but
//! one (`people.note` stays unconfirmed, so it counts as `personal`), and inserts seven people
//! and six orders. `agent_out` (the agent of `member`, scope `Private`, `max_role: manage`) is
//! the agent of the cases; `member_view` and `member_edit` are the people without `manage`.
//!
//! Most `personal` and `secret` values of the fixture hold a unique marker:
//! `ruum-raw-<tag>-p<n>` (personal) and `ruum-raw-<tag>-s<n>` (secret), with a fresh `<tag>`
//! per case run. Three values have no marker by design (review-T-2 N-4): `handle` (personal),
//! `pin` (secret) and the shared email of person 7 (personal). Their normal copies in `orders`
//! must be readable by the agent. [`leaks`] does not catch a leak of them; only the explicit
//! checks of each case do. Do not rely on the helper for these columns. [`Ctx::send`] checks every answer with [`leaks`]: the body as sent, every
//! string inside the JSON body after decoding (so an escaped marker counts too), and every
//! header. A marker that the caller may not see fails the case, whatever the status. At the end
//! of a case on Postgres, the same check runs on the stored rows of `database_queries`, the
//! parameters of `database_approvals` and the status of `typed_nodes`: no marker may be there
//! at all, for anyone (design 11.2, decision 0004-8).
//!
//! The oracle cases need more than a missing marker, because a count or an order can leak a
//! value bit by bit. They hold values without markers that a person can see (the `handle` and
//! `pin` columns and their normal copies in `orders`), and each oracle runs as a person first
//! (the control: the real comparison finds the rows) and then as the agent (the lens compares
//! pseudonyms, so it finds nothing).
//!
//! # Planned
//!
//! A case runs only when none of its operations is planned (`x-ruum-status: planned`, decision
//! 0004-21), when it waits for no card ([`Case::waits`]), when its engine exists in `Services`
//! (SQLite: [`SQLITE_WAITS_FOR`], card E-3), when the privacy filter of its engine exists if
//! it needs one ([`Filter`], [`FILTER_WAITS_FOR`]: Q-2 for Postgres, Q-3 for SQLite), and not
//! on the memory backend ([`Memory`]: no case runs on the memory engine, which is SQLite in
//! memory with real values). Otherwise it reports `planned` with all its reasons; it is never
//! skipped silently. A planned case passes in both
//! modes, also with `RUUM_REQUIRE_IMPLEMENTED=1`. An operation that is not planned but answers
//! `501` makes the case `pending`.
//!
//! Set `RUUM_PRIVACY_CASES=<prefix>[,<prefix>...]` to run only the cases whose ID starts with
//! one of the prefixes (the mutation rules use it).
//!
//! # Pseudonyms
//!
//! The suite never needs the tenant key. [`pseudonym_id`] checks the shape against a pseudonym
//! that `ruum_core::privacy` makes with a fixed test key (`p_` plus 12 hex characters). "Same
//! value, same pseudonym" and "a branch differs from its source" compare two answers.

use crate::backend::{Backend, Kind};
use crate::client::{self, RequestSpec, Response};
use crate::fixtures::{self, Who, World};
use crate::report::{Outcome, Report};
use axum::Router;
use futures::stream::{self, StreamExt};
use serde_json::{Value, json};
use sqlx::postgres::PgConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

/// The variable that limits the run to some cases (ID prefixes, comma separated).
pub const CASES_ENV: &str = "RUUM_PRIVACY_CASES";

/// The card that brings a SQLite engine into `Services`. While it is `Some`, every SQLite case
/// is planned with this card in its reason. Card E-3 sets it to `None`. The SQLite cases that
/// need the privacy filter then still wait for Q-3 ([`FILTER_WAITS_FOR`]).
pub const SQLITE_WAITS_FOR: Option<&str> = Some("E-3");

/// The cards that build the privacy filter of each engine (review-T-2 R-2): the lens views,
/// the role kind per caller and grant, the markers from result column classes,
/// `class_read_required` and the sanitized errors for agents. Q-1 builds `queryDatabase` for
/// people only ("Out of scope: agents (Q-2, Q-3)"), and Q-2 and Q-3 remove no planned mark.
/// So every case with [`Filter::Needed`] waits here for the card of its engine, on both
/// backends. Q-2 removed the Postgres entry when it landed; Q-3 removes the SQLite entry.
pub const FILTER_WAITS_FOR: &[(Engine, &str)] = &[(Engine::Sqlite, "Q-3")];

/// The card in [`FILTER_WAITS_FOR`] for `engine`, while it has one.
pub fn filter_waits_for(engine: Engine) -> Option<&'static str> {
    FILTER_WAITS_FOR
        .iter()
        .find(|(e, _)| *e == engine)
        .map(|(_, card)| *card)
}

/// The operations every case uses to build its fixture. The fixture's tables come from
/// `applyDatabaseMigration` once it is built (card D-2); while it is planned, `member` (a person
/// with `manage`) runs the same statements through `queryDatabase`, which takes a schema change
/// through the engine's migration path and the catalog sync (Q-1 wave decision 3, card Q-2).
/// Both paths end with the same confirmation of the classes, so the cases see the same
/// database. A case that needs a named migration itself (its status, its error, a migration on a
/// branch) lists `applyDatabaseMigration` in its own operations.
pub const FIXTURE_OPERATIONS: &[&str] = &[
    "createDatabase",
    "getDatabase",
    "confirmColumnClasses",
    "queryDatabase",
];

/// The longest wait for a database to become `ready`.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

// ------------------------------------------------------------------------------- the cases

/// A database engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Engine {
    Postgres,
    Sqlite,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Sqlite => "sqlite",
        }
    }

    /// The schema of the database's own tables.
    pub fn schema(self) -> &'static str {
        match self {
            Engine::Postgres => "public",
            Engine::Sqlite => "main",
        }
    }

    /// The placeholder of parameter `n` (1-based).
    pub fn ph(self, n: usize) -> String {
        match self {
            Engine::Postgres => format!("${n}"),
            Engine::Sqlite => format!("?{n}"),
        }
    }

    fn manifest(self) -> Value {
        match self {
            Engine::Postgres => {
                json!({ "engine": "postgres", "version": "17", "plan": "shared-small" })
            }
            Engine::Sqlite => json!({ "engine": "sqlite", "version": "3", "plan": "shared-small" }),
        }
    }
}

/// The groups of the card's "Done when".
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// One case per operation and data path: query, schema, log, approval, branch, copy.
    Operation,
    /// Clauses that would leak a value bit by bit without the lens.
    Oracle,
    /// Writes that would copy a restricted value into a column read as `normal`.
    Write,
    /// Migrations that would carry a confirmation over to another column.
    Rename,
    /// Role and session statements (Postgres).
    Role,
    /// Files outside the database (SQLite).
    File,
    /// The pseudonym function and its key.
    Pseudonym,
    /// Error texts, migration errors, the status message and the query log.
    Leak,
    /// Statistics and plans.
    SideChannel,
    /// Pseudonyms of chosen values (decisions 0004-24, 0004-25).
    PseudonymOracle,
    /// Database exports (decision 0004-29).
    Export,
}

/// What the memory backend does with a case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Memory {
    /// The case runs on the memory backend too.
    Runs,
    /// The case is planned on the memory backend, for this reason.
    Planned(&'static str),
}

/// The lens of the memory engine reads real values (review-E-1 N-1, review-E-2 N-3).
pub const MEMORY_LENS: Memory = Memory::Planned("the memory lens is not masked (Q-2)");
/// The memory engine is SQLite in memory, also for an `@postgres` run: the Postgres fixture
/// (the `public` schema, `::jsonb`) does not run there, and the engine reads real values
/// (review-T-2 R-1). Its own doc says that no privacy test may run on it.
pub const MEMORY_ENGINE: Memory = Memory::Planned(
    "the memory engine is SQLite in memory: the Postgres fixture does not run there, and no \
     privacy test may run on it (E-1)",
);
/// The memory engine has no roles, files or catalog identity.
pub const MEMORY_FAKE: Memory = Memory::Planned(
    "the memory engine is a fake: no roles, files, statistics or catalog identity (E-1)",
);

/// Whether a case needs the privacy filter of its engine ([`FILTER_WAITS_FOR`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    /// The case sends a statement as an agent, or reads a masked value or a marker: it waits
    /// for the filter card of its engine. Every [`MEMORY_LENS`] case needs the filter.
    Needed,
    /// The case reads only the schema, the status, the migration list or the engine's own
    /// refusals (files), so it runs as soon as its operations and engine exist.
    NotNeeded,
}

const FILTER: Filter = Filter::Needed;
const NO_FILTER: Filter = Filter::NotNeeded;

/// A card a case waits for, beyond the planned marks of its operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wait {
    /// The card that removes this entry when it lands.
    pub card: &'static str,
    pub reason: &'static str,
}

const WAIT_EXPORT: Wait = Wait {
    card: "panel",
    reason: "no public operation starts a database export (exportTypedNode is a provider \
             operation); contract gap recorded for the panel",
};

/// Why a case stopped early.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stop {
    Failed(String),
    /// An operation that is not planned answered `501`.
    Pending(String),
    /// The case found at run time that it cannot run yet.
    Planned(String),
}

/// The result of one step of a case.
pub type Step<T> = Result<T, Stop>;

/// The future of a case body.
pub type CaseFuture<'a> = Pin<Box<dyn Future<Output = Step<()>> + Send + 'a>>;

/// The body of a case.
pub type Runner = for<'a> fn(&'a mut Ctx) -> CaseFuture<'a>;

/// One case of the suite.
#[derive(Clone, Copy)]
pub struct Case {
    /// `<group>.<name>`, unique.
    pub id: &'static str,
    pub group: Group,
    pub title: &'static str,
    /// The contract operations of the case, beyond [`FIXTURE_OPERATIONS`].
    pub operations: &'static [&'static str],
    pub engines: &'static [Engine],
    pub memory: Memory,
    pub filter: Filter,
    pub waits: &'static [Wait],
    pub run: Runner,
}

impl Case {
    /// Every operation the case needs: the fixture's and its own.
    pub fn all_operations(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = FIXTURE_OPERATIONS.to_vec();
        for op in self.operations {
            if !v.contains(op) {
                v.push(op);
            }
        }
        v
    }

    /// Why the case cannot run on `engine` and `backend` now, or `None` when it can run.
    pub fn planned_reason(&self, engine: Engine, backend: Kind) -> Option<String> {
        let mut why = Vec::new();
        let planned: Vec<&str> = self
            .all_operations()
            .into_iter()
            .filter(|op| crate::contract::is_planned(op))
            .collect();
        if !planned.is_empty() {
            why.push(format!("operations planned: {}", planned.join(", ")));
        }
        for w in self.waits {
            why.push(format!("waits for {}: {}", w.card, w.reason));
        }
        if self.filter == Filter::Needed
            && let Some(card) = filter_waits_for(engine)
        {
            why.push(format!(
                "waits for {card}: the privacy filter of {} (lens views, role kind per caller, \
                 markers, sanitized agent errors)",
                engine.name()
            ));
        }
        if engine == Engine::Sqlite
            && let Some(card) = SQLITE_WAITS_FOR
        {
            why.push(format!(
                "no SQLite engine in Services yet ({card}; Q-3 builds the SQLite lens)"
            ));
        }
        if backend == Kind::Memory
            && let Memory::Planned(r) = self.memory
        {
            why.push(r.to_string());
        }
        (!why.is_empty()).then(|| why.join("; "))
    }
}

macro_rules! fail {
    ($($t:tt)*) => {
        return Err(Stop::Failed(format!($($t)*)))
    };
}

macro_rules! ensure {
    ($cond:expr, $($t:tt)*) => {
        if !$cond {
            fail!($($t)*);
        }
    };
}

const PG: &[Engine] = &[Engine::Postgres];
const SQ: &[Engine] = &[Engine::Sqlite];
const BOTH: &[Engine] = &[Engine::Postgres, Engine::Sqlite];

#[allow(clippy::too_many_arguments)]
fn case(
    id: &'static str,
    group: Group,
    title: &'static str,
    operations: &'static [&'static str],
    engines: &'static [Engine],
    memory: Memory,
    filter: Filter,
    waits: &'static [Wait],
    run: Runner,
) -> Case {
    Case {
        id,
        group,
        title,
        operations,
        engines,
        memory,
        filter,
        waits,
        run,
    }
}

/// All cases of the suite.
pub fn cases() -> Vec<Case> {
    use Group::*;
    let c = case;
    vec![
        // ---------------------------------------------------------------- operations
        c(
            "query.plain_column",
            Operation,
            "queryDatabase: plain personal, secret and unconfirmed columns are masked for the agent",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_plain_column,
        ),
        c(
            "query.expression",
            Operation,
            "queryDatabase: expressions over personal and secret columns are masked; constants are not",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_expression,
        ),
        c(
            "query.join",
            Operation,
            "queryDatabase: a join of two personal columns works on pseudonyms",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_join,
        ),
        c(
            "query.aggregate",
            Operation,
            "queryDatabase: aggregates over personal columns give pseudonyms or counts, never values",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_aggregate,
        ),
        c(
            "query.jsonb_ruum",
            Operation,
            "queryDatabase: a jsonb object with $ruum comes back as a literal, never as a marker",
            &[],
            PG,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_jsonb_ruum,
        ),
        c(
            "query.same_value_same_pseudonym",
            Operation,
            "queryDatabase: the same value gives the same pseudonym, other values other pseudonyms",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_same_value,
        ),
        c(
            "query.unconfirmed_is_personal",
            Operation,
            "an unconfirmed column counts as personal in the schema, the column list and the lens",
            &["getDatabaseSchema", "listDatabaseColumns"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_unconfirmed,
        ),
        c(
            "query.secret_without_manage",
            Operation,
            "people without manage never see a secret, also not through a filter",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_secret_without_manage,
        ),
        c(
            "query.read_personal_grant",
            Operation,
            "a read_personal grant shows personal values to the agent, never secrets",
            &["createDatabaseAccessGrant"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::query_read_personal,
        ),
        c(
            "schema.names_only",
            Operation,
            "getDatabaseSchema: names, types and classes, never a value",
            &["getDatabaseSchema"],
            BOTH,
            MEMORY_ENGINE,
            NO_FILTER,
            &[],
            cases::schema_names_only,
        ),
        c(
            "queries.agent_log",
            Operation,
            "listDatabaseQueries: an agent sees its own entries, with hidden parameters",
            &["listDatabaseQueries"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::queries_agent_log,
        ),
        c(
            "approval.params_hidden",
            Operation,
            "getDatabaseApproval: parameters are hidden markers, never values or pseudonyms",
            &["createDatabaseAccessGrant", "getDatabaseApproval"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::approval_params_hidden,
        ),
        c(
            "branch.masked",
            Operation,
            "a masked branch holds no raw value, for the agent and for its manager",
            &["createDatabaseBranch"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::branch_masked,
        ),
        c(
            "copy.agent",
            Operation,
            "an agent's copy of a database holds masked data only",
            &["copyNode"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::copy_agent,
        ),
        // ---------------------------------------------------------------- oracles
        c(
            "oracle.like_count",
            Oracle,
            "count(*) ... WHERE email LIKE 'a%' compares the pseudonym",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_like_count,
        ),
        c(
            "oracle.eq_param",
            Oracle,
            "WHERE email = $1 with the real value finds nothing",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_eq_param,
        ),
        c(
            "oracle.order_by",
            Oracle,
            "ORDER BY email orders by the pseudonym",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_order_by,
        ),
        c(
            "oracle.group_by",
            Oracle,
            "GROUP BY email groups by the pseudonym",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_group_by,
        ),
        c(
            "oracle.min",
            Oracle,
            "min(email) is the smallest pseudonym",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_min,
        ),
        c(
            "oracle.length",
            Oracle,
            "length(email) is the length of the pseudonym",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_length,
        ),
        c(
            "oracle.join_personal_normal",
            Oracle,
            "a join of a personal column with a normal column holding the same text finds nothing",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_join_personal_normal,
        ),
        c(
            "oracle.join_secret_normal",
            Oracle,
            "a join of a secret column with a normal column finds nothing, also for a person without manage",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::oracle_join_secret_normal,
        ),
        // ---------------------------------------------------------------- writes
        c(
            "write.update_set",
            Write,
            "UPDATE t SET normal_col = personal_col: 403 class_read_required on live and on a draft branch",
            &["createDatabaseAccessGrant", "createDatabaseBranch"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::write_update_set,
        ),
        c(
            "write.insert_select",
            Write,
            "INSERT ... SELECT personal_col: 403 class_read_required on live and on a draft branch",
            &["createDatabaseAccessGrant", "createDatabaseBranch"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::write_insert_select,
        ),
        c(
            "write.returning",
            Write,
            "RETURNING personal_col: 403 class_read_required on live and on a draft branch",
            &["createDatabaseAccessGrant", "createDatabaseBranch"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::write_returning,
        ),
        // ---------------------------------------------------------------- renames
        c(
            "rename.onto_dropped_normal",
            Rename,
            "renaming email to the name of a dropped confirmed-normal column leaves it unconfirmed",
            &["listDatabaseColumns"],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::rename_onto_dropped_normal,
        ),
        c(
            "rename.table_onto_dropped",
            Rename,
            "renaming a table onto a dropped table's name leaves its columns unconfirmed",
            &["listDatabaseColumns"],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::rename_table_onto_dropped,
        ),
        c(
            "rename.retype",
            Rename,
            "retyping a column leaves it unconfirmed",
            &["listDatabaseColumns"],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::rename_retype,
        ),
        c(
            "rename.drop_add",
            Rename,
            "dropping and adding a column of the same name leaves it unconfirmed",
            &["listDatabaseColumns"],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::rename_drop_add,
        ),
        // ---------------------------------------------------------------- roles
        c(
            "role.reset_role",
            Role,
            "RESET ROLE reaches no other role",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::role_reset_role,
        ),
        c(
            "role.set_role_owner",
            Role,
            "SET ROLE <owner> reaches no other role",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::role_set_role_owner,
        ),
        c(
            "role.set_role_dbhost",
            Role,
            "SET ROLE ruum_dbhost reaches no other role",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::role_set_role_dbhost,
        ),
        c(
            "role.set_config",
            Role,
            "set_config('role', ...) reaches no other role",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::role_set_config,
        ),
        c(
            "role.do_block",
            Role,
            "a DO block with SET ROLE or set_config reaches no other role",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::role_do_block,
        ),
        // ---------------------------------------------------------------- files
        c(
            "file.attach_other_tenant",
            File,
            "ATTACH DATABASE of a file of tenant B is refused",
            &[],
            SQ,
            MEMORY_FAKE,
            NO_FILTER,
            &[],
            cases::file_attach_other_tenant,
        ),
        c(
            "file.vacuum_into",
            File,
            "VACUUM INTO '<path>' is refused and writes no file",
            &[],
            SQ,
            MEMORY_FAKE,
            NO_FILTER,
            &[],
            cases::file_vacuum_into,
        ),
        c(
            "file.load_extension",
            File,
            "load_extension(...) is refused",
            &[],
            SQ,
            MEMORY_FAKE,
            NO_FILTER,
            &[],
            cases::file_load_extension,
        ),
        c(
            "file.pragma",
            File,
            "a PRAGMA outside the allow-list is refused",
            &[],
            SQ,
            MEMORY_FAKE,
            NO_FILTER,
            &[],
            cases::file_pragma,
        ),
        // ---------------------------------------------------------------- pseudonyms
        c(
            "pseudonym.direct_call",
            Pseudonym,
            "a direct call of ruum_mask.pseudonym or ruum_pseudonym fails",
            &[],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::pseudonym_direct_call,
        ),
        c(
            "pseudonym.constant",
            Pseudonym,
            "SELECT 'guess' comes back unmasked and is not a pseudonym",
            &[],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::pseudonym_constant,
        ),
        c(
            "pseudonym.key_unreadable",
            Pseudonym,
            "the key is not readable from an agent role (ruum_mask.key, pg_seclabels, settings)",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::pseudonym_key_unreadable,
        ),
        // ---------------------------------------------------------------- leaks
        c(
            "leak.unique_violation",
            Leak,
            "a unique violation from an agent and from a person without manage shows no value",
            &["createDatabaseAccessGrant"],
            BOTH,
            MEMORY_ENGINE,
            FILTER,
            &[],
            cases::leak_unique_violation,
        ),
        c(
            "leak.check_violation",
            Leak,
            "a check violation from an agent and from a person without manage shows no value",
            &["createDatabaseAccessGrant"],
            BOTH,
            MEMORY_ENGINE,
            FILTER,
            &[],
            cases::leak_check_violation,
        ),
        c(
            "leak.failed_migration",
            Leak,
            "a failed migration in listDatabaseMigrations shows no value to a viewer or an agent",
            &["applyDatabaseMigration", "listDatabaseMigrations"],
            BOTH,
            MEMORY_ENGINE,
            NO_FILTER,
            &[],
            cases::leak_failed_migration,
        ),
        c(
            "leak.status_message",
            Leak,
            "the status message holds no row value, for anyone",
            &["applyDatabaseMigration"],
            BOTH,
            MEMORY_ENGINE,
            NO_FILTER,
            &[],
            cases::leak_status_message,
        ),
        c(
            "leak.log_literals",
            Leak,
            "a person's literals are not in listDatabaseQueries; an agent with manage sees only its own entries",
            &["listDatabaseQueries"],
            BOTH,
            MEMORY_ENGINE,
            FILTER,
            &[],
            cases::leak_log_literals,
        ),
        // ---------------------------------------------------------------- side channels
        c(
            "side.pg_stats",
            SideChannel,
            "pg_stats from an agent role shows no value",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::side_pg_stats,
        ),
        c(
            "side.explain",
            SideChannel,
            "EXPLAIN from an agent role shows no value",
            &[],
            PG,
            MEMORY_FAKE,
            FILTER,
            &[],
            cases::side_explain,
        ),
        // ---------------------------------------------------------------- pseudonym oracles
        c(
            "poracle.log_param",
            PseudonymOracle,
            "a parameter in listDatabaseQueries is a hidden marker, never the lens pseudonym",
            &["listDatabaseQueries"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::poracle_log_param,
        ),
        c(
            "poracle.approval_param",
            PseudonymOracle,
            "a parameter in getDatabaseApproval is a hidden marker, never the lens pseudonym",
            &["createDatabaseAccessGrant", "getDatabaseApproval"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::poracle_approval_param,
        ),
        c(
            "poracle.masked_branch_key",
            PseudonymOracle,
            "a masked branch's lens gives other pseudonyms than its source's (agent rows, migration, DO)",
            &["createDatabaseBranch", "applyDatabaseMigration"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::poracle_masked_branch,
        ),
        c(
            "poracle.schema_only_branch_key",
            PseudonymOracle,
            "a schema_only branch's lens gives other pseudonyms than its source's",
            &["createDatabaseBranch", "applyDatabaseMigration"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::poracle_schema_only_branch,
        ),
        c(
            "poracle.copy_keeps_branch_key",
            PseudonymOracle,
            "a copy of a masked branch keeps the branch's key",
            &["createDatabaseBranch", "copyNode", "applyDatabaseMigration"],
            BOTH,
            MEMORY_LENS,
            FILTER,
            &[],
            cases::poracle_copy_keeps_key,
        ),
        // ---------------------------------------------------------------- exports
        c(
            "export.person_full",
            Export,
            "a person's full export holds no key and no Ruum schema",
            &[],
            BOTH,
            MEMORY_FAKE,
            NO_FILTER,
            &[WAIT_EXPORT],
            cases::export_person_full,
        ),
        c(
            "export.person_masked",
            Export,
            "a person's masked export holds no key, no Ruum schema and no raw value",
            &[],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[WAIT_EXPORT],
            cases::export_person_masked,
        ),
        c(
            "export.agent",
            Export,
            "an agent gets a masked export only, with no key, no Ruum schema and no raw value",
            &[],
            BOTH,
            MEMORY_FAKE,
            FILTER,
            &[WAIT_EXPORT],
            cases::export_agent,
        ),
    ]
}

// --------------------------------------------------------------------------- leak checks

/// The markers of `forbidden` that occur in `text`.
pub fn leaks_in_text(text: &str, forbidden: &[String]) -> Vec<String> {
    forbidden
        .iter()
        .filter(|m| !m.is_empty() && text.contains(m.as_str()))
        .cloned()
        .collect()
}

/// Every string of a JSON value (keys and values), decoded, one per line.
pub fn json_strings(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::String(s) => {
                out.push_str(s);
                out.push('\n');
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            Value::Object(m) => {
                for (k, x) in m {
                    out.push_str(k);
                    out.push('\n');
                    walk(x, out);
                }
            }
            Value::Number(n) => {
                out.push_str(&n.to_string());
                out.push('\n');
            }
            _ => {}
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out
}

/// The markers of `forbidden` anywhere in an answer: the body as sent, every decoded string of
/// a JSON body (the error `message` and `hint`, cells, marker JSON), and every header value.
/// The one helper of every case.
pub fn leaks(res: &Response, forbidden: &[String]) -> Vec<String> {
    let mut text = res.text();
    if let Some(v) = res.json() {
        text.push('\n');
        text.push_str(&json_strings(&v));
    }
    let mut found = leaks_in_text(&text, forbidden);
    found.sort();
    found.dedup();
    found
}

// ----------------------------------------------------------------------------- pseudonyms

/// A pseudonym made with a fixed test key: the shape of the platform's pseudonyms. Never the
/// tenant key.
pub fn shape_example() -> String {
    use ruum_core::privacy::{HmacPseudonymizer, PseudonymKey};
    HmacPseudonymizer::new(PseudonymKey::from_bytes([7u8; 32]))
        .pseudonym_of_canonical("shape")
        .id()
        .to_string()
}

/// Whether `id` has the shape of the platform's pseudonyms (`p_` plus 12 hex characters).
pub fn has_pseudonym_shape(id: &str) -> bool {
    id.len() == shape_example().len() && ruum_core::privacy::Pseudonym::parse(id).is_some()
}

/// The `id` of a pseudonym marker cell with the platform's shape.
pub fn pseudonym_id(cell: &Value) -> Option<String> {
    match serde_json::from_value::<ruum_core::privacy::Marker>(cell.clone()) {
        Ok(ruum_core::privacy::Marker::Pseudonym(p)) if has_pseudonym_shape(p.id()) => {
            Some(p.id().to_string())
        }
        _ => None,
    }
}

/// Whether a cell is the hidden-value marker of `class` (`secret` or `personal`).
pub fn is_hidden(cell: &Value, class: &str) -> bool {
    cell.get("$ruum").and_then(Value::as_str) == Some("hidden")
        && cell.get("class").and_then(Value::as_str) == Some(class)
        && cell.as_object().is_some_and(|m| m.len() == 2)
}

/// Whether a cell is masked: a pseudonym marker, a hidden marker, or the text of a pseudonym
/// (an expression over a lens column, for example `lower(email)`).
pub fn is_masked(cell: &Value) -> bool {
    pseudonym_id(cell).is_some()
        || is_hidden(cell, "personal")
        || is_hidden(cell, "secret")
        || cell.as_str().is_some_and(has_pseudonym_shape)
}

// ------------------------------------------------------------------------------ the fixture

/// What data a database holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    /// Real data: a live database or a `full` branch.
    Real,
    /// A `masked` or `schema_only` branch, or a masked copy: no raw value for anyone.
    Masked,
}

/// A database of a case.
#[derive(Clone, Debug)]
pub struct Db {
    /// The node ID.
    pub id: String,
    pub data: Data,
}

/// One person of the fixture, with its real values.
#[derive(Clone, Debug)]
pub struct Person {
    pub id: i64,
    pub email: String,
    pub handle: String,
    pub name: Option<String>,
    pub pin: String,
    pub token: Option<String>,
    pub nick: String,
    pub score: i64,
    pub profile: Option<Value>,
    pub note: Option<String>,
}

/// The fixture of a case: the live database and its rows.
#[derive(Clone, Debug)]
pub struct Fixture {
    pub db: Db,
    pub people: Vec<Person>,
}

impl Fixture {
    /// The person whose email is the value shared with the branch cases (no marker).
    pub fn shared(&self) -> &Person {
        self.people.last().expect("the fixture has people")
    }
}

/// The DDL of the fixture.
fn fixture_ddl(engine: Engine) -> String {
    let json = match engine {
        Engine::Postgres => "jsonb",
        Engine::Sqlite => "text",
    };
    format!(
        "create table people (\n\
         id integer primary key,\n\
         email text not null unique,\n\
         handle text not null,\n\
         name text,\n\
         pin text,\n\
         token text,\n\
         nick text,\n\
         score integer not null check (score >= 0),\n\
         profile {json},\n\
         note text\n\
         );\n\
         create table orders (\n\
         id integer primary key,\n\
         person_email text not null,\n\
         handle_copy text not null,\n\
         pin_copy text not null,\n\
         amount integer not null,\n\
         label text,\n\
         meta {json}\n\
         );\n"
    )
}

/// The confirmed classes of the fixture. `people.note` is left out: it stays unconfirmed.
const CLASSES: &[(&str, &str, &str)] = &[
    ("people", "id", "normal"),
    ("people", "email", "personal"),
    ("people", "handle", "personal"),
    ("people", "name", "personal"),
    ("people", "pin", "secret"),
    ("people", "token", "secret"),
    ("people", "nick", "normal"),
    ("people", "score", "normal"),
    ("people", "profile", "personal"),
    ("orders", "id", "normal"),
    ("orders", "person_email", "personal"),
    ("orders", "handle_copy", "normal"),
    ("orders", "pin_copy", "normal"),
    ("orders", "amount", "normal"),
    ("orders", "label", "normal"),
    ("orders", "meta", "normal"),
];

// --------------------------------------------------------------------------------- context

/// The state of one case run.
pub struct Ctx {
    pub case: &'static str,
    pub engine: Engine,
    pub backend: Kind,
    pub app: Router,
    pub w: World,
    /// The marker tag of this run.
    pub tag: String,
    /// The databases where `agent_out` holds a `read_personal` grant.
    read_personal: BTreeSet<String>,
    fixture: Option<Fixture>,
    migration_no: usize,
}

impl Ctx {
    fn new(case: &'static str, engine: Engine, backend: Kind, app: Router, w: World) -> Ctx {
        let tag = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
        Ctx {
            case,
            engine,
            backend,
            app,
            w,
            tag,
            read_personal: BTreeSet::new(),
            fixture: None,
            migration_no: 1,
        }
    }

    /// The marker of personal value `n`.
    pub fn personal_marker(&self, n: usize) -> String {
        format!("ruum-raw-{}-p{n}", self.tag)
    }

    /// The marker of secret value `n`.
    pub fn secret_marker(&self, n: usize) -> String {
        format!("ruum-raw-{}-s{n}", self.tag)
    }

    fn personal_prefix(&self) -> String {
        format!("ruum-raw-{}-p", self.tag)
    }

    fn secret_prefix(&self) -> String {
        format!("ruum-raw-{}-s", self.tag)
    }

    /// Both prefixes: nothing of the fixture's restricted values may appear.
    pub fn all_markers(&self) -> Vec<String> {
        vec![self.personal_prefix(), self.secret_prefix()]
    }

    /// The markers that `who` may not see in answers about `db`.
    pub fn forbidden(&self, who: Who, db: &Db) -> Vec<String> {
        if db.data == Data::Masked {
            return self.all_markers();
        }
        let personal_ok = if who.is_agent() {
            self.read_personal.contains(&db.id)
        } else {
            who.is_user()
        };
        let secret_ok = matches!(who, Who::Owner | Who::Admin | Who::Member);
        let mut v = Vec::new();
        if !personal_ok {
            v.push(self.personal_prefix());
        }
        if !secret_ok {
            v.push(self.secret_prefix());
        }
        v
    }

    fn path(&self, db: &Db, rest: &str) -> String {
        format!("/v1/databases/{}{rest}", db.id)
    }

    /// Sends `spec` as `who` of tenant A and checks the answer for markers that `who` may not
    /// see about `db`. A `501` stops the case as pending.
    pub async fn send(&mut self, who: Who, db: &Db, spec: RequestSpec) -> Step<Response> {
        let forbidden = self.forbidden(who, db);
        self.send_checked(who, spec, &forbidden).await
    }

    /// Sends `spec` as `who` of tenant A and checks the answer against `forbidden`.
    pub async fn send_checked(
        &mut self,
        who: Who,
        spec: RequestSpec,
        forbidden: &[String],
    ) -> Step<Response> {
        let spec = spec.tenant(self.w.a.id);
        let label = spec.label();
        let principal = self.w.a.principal(who).clone();
        let res = client::send(&self.app, &spec, &principal).await;
        let found = leaks(&res, forbidden);
        ensure!(
            found.is_empty(),
            "{label} as {} ({}) leaks {found:?}: {}",
            who.name(),
            res.status,
            excerpt(&res)
        );
        if res.is_not_implemented() {
            return Err(Stop::Pending(format!("{label} answers 501")));
        }
        Ok(res)
    }

    /// Runs one statement.
    pub async fn query(
        &mut self,
        who: Who,
        db: &Db,
        sql: &str,
        params: Vec<Value>,
        write: bool,
    ) -> Step<Response> {
        let body = json!({
            "sql": sql,
            "params": params,
            "mode": if write { "write" } else { "read" },
        });
        let spec = RequestSpec::post(self.path(db, "/query")).json(body);
        self.send(who, db, spec).await
    }

    /// Runs one read statement and expects `200`.
    pub async fn rows(&mut self, who: Who, db: &Db, sql: &str, params: Vec<Value>) -> Step<Rows> {
        let res = self.query(who, db, sql, params, false).await?;
        want(&res, 200, None, &format!("{sql} as {}", who.name()))?;
        Rows::parse(&res)
    }

    /// Runs one write statement and expects `200`.
    pub async fn write_ok(&mut self, who: Who, db: &Db, sql: &str, params: Vec<Value>) -> Step<()> {
        let res = self.query(who, db, sql, params, true).await?;
        want(&res, 200, None, &format!("{sql} as {}", who.name()))
    }

    /// One integer of a one-row answer.
    pub async fn count(&mut self, who: Who, db: &Db, sql: &str, params: Vec<Value>) -> Step<i64> {
        let r = self.rows(who, db, sql, params).await?;
        r.int(0, 0)
    }

    /// Applies a migration as `who`.
    pub async fn migrate(&mut self, who: Who, db: &Db, up: &str, classes: Value) -> Step<Response> {
        self.migration_no += 1;
        let name = format!("{:04}_privacy_case", self.migration_no);
        let spec = RequestSpec::post(self.path(db, "/migrations")).json(json!({
            "name": name,
            "up": up,
            "data_classes": classes,
        }));
        self.send(who, db, spec).await
    }

    /// Applies a migration as `member` and expects it to be applied. While
    /// `applyDatabaseMigration` is planned (card D-2), `member` runs each statement of `up`
    /// through `queryDatabase` instead ([`Ctx::schema_change`]).
    pub async fn migrate_ok(&mut self, db: &Db, up: &str) -> Step<()> {
        if crate::contract::is_planned("applyDatabaseMigration") {
            return self.schema_change(db, up).await;
        }
        let res = self.migrate(Who::Member, db, up, json!({})).await?;
        want(&res, 200, None, "the migration of member")?;
        self.wait_ready(db).await
    }

    /// Runs each statement of `up` (split at `;`; the statements of the suite hold no `;` in a
    /// literal) as `member` through `queryDatabase` in write mode: a person with `manage` runs a
    /// schema change through the engine's migration path and the catalog sync (Q-1 wave
    /// decision 3), so the classes follow column identity as after a migration.
    pub async fn schema_change(&mut self, db: &Db, up: &str) -> Step<()> {
        for statement in up.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            self.write_ok(Who::Member, db, statement, vec![]).await?;
        }
        self.wait_ready(db).await
    }

    /// Polls `getDatabase` as `member` until `ready`.
    pub async fn wait_ready(&mut self, db: &Db) -> Step<()> {
        let start = std::time::Instant::now();
        loop {
            let spec = RequestSpec::get(self.path(db, ""));
            let res = self.send(Who::Member, db, spec).await?;
            want(&res, 200, None, "getDatabase")?;
            let state = res
                .json()
                .and_then(|v| v["status"]["state"].as_str().map(str::to_string))
                .unwrap_or_default();
            match state.as_str() {
                "ready" => return Ok(()),
                "provisioning" | "updating" if start.elapsed() < READY_TIMEOUT => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => fail!(
                    "database {} is `{other}`, not ready: {}",
                    db.id,
                    excerpt(&res)
                ),
            }
        }
    }

    /// Creates a database in `Private` of tenant A as `member` and waits until it is ready.
    pub async fn create_database(&mut self, name: &str) -> Step<Db> {
        let parent = self.w.a.private;
        let spec = RequestSpec::post("/v1/databases").json(json!({
            "parent_id": parent,
            "name": name,
            "manifest": self.engine.manifest(),
        }));
        let db0 = Db {
            id: String::new(),
            data: Data::Real,
        };
        let res = self.send(Who::Member, &db0, spec).await?;
        want(&res, 202, None, "createDatabase")?;
        let id = res
            .json()
            .and_then(|v| v["database"]["node"]["id"].as_str().map(str::to_string))
            .ok_or_else(|| {
                Stop::Failed(format!("createDatabase gave no node ID: {}", excerpt(&res)))
            })?;
        let db = Db {
            id,
            data: Data::Real,
        };
        self.wait_ready(&db).await?;
        Ok(db)
    }

    /// The fixture of the case, built at the first call.
    pub async fn fixture(&mut self) -> Step<Fixture> {
        if let Some(f) = &self.fixture {
            return Ok(f.clone());
        }
        let db = self
            .create_database(&format!("privacy {}", self.case))
            .await?;
        let classes: serde_json::Map<String, Value> = CLASSES
            .iter()
            .map(|(t, c, k)| (format!("{}.{t}.{c}", self.engine.schema()), json!(k)))
            .collect();
        if crate::contract::is_planned("applyDatabaseMigration") {
            self.schema_change(&db, &fixture_ddl(self.engine)).await?;
        } else {
            let res = self
                .migrate(
                    Who::Member,
                    &db,
                    &fixture_ddl(self.engine),
                    Value::Object(classes),
                )
                .await?;
            want(&res, 200, None, "the fixture migration")?;
            self.wait_ready(&db).await?;
        }
        self.confirm(&db, CLASSES).await?;
        let people = self.people();
        for p in &people {
            self.insert_person(&db, p).await?;
        }
        for p in people.iter().take(6) {
            let e = self.engine;
            let sql = format!(
                "insert into orders (id, person_email, handle_copy, pin_copy, amount, label, meta) \
                 values ({}, {}, {}, {}, {}, {}, {})",
                e.ph(1),
                e.ph(2),
                e.ph(3),
                e.ph(4),
                e.ph(5),
                e.ph(6),
                self.json_param(7)
            );
            let meta = json!({ "$ruum": "hidden", "class": "secret" }).to_string();
            self.write_ok(
                Who::Member,
                &db,
                &sql,
                vec![
                    json!(p.id),
                    json!(p.email),
                    json!(p.handle),
                    json!(p.pin),
                    json!(p.id),
                    json!(format!("order-{}", p.id)),
                    json!(meta),
                ],
            )
            .await?;
        }
        let f = Fixture { db, people };
        self.fixture = Some(f.clone());
        Ok(f)
    }

    /// A JSON parameter: cast to `jsonb` on Postgres. The fixture runs on real engines only:
    /// no case runs on the memory backend ([`MEMORY_ENGINE`]).
    fn json_param(&self, n: usize) -> String {
        match self.engine {
            Engine::Postgres => format!("{}::jsonb", self.engine.ph(n)),
            Engine::Sqlite => self.engine.ph(n),
        }
    }

    /// The seven people: six with markers and one with the shared value (no marker).
    fn people(&self) -> Vec<Person> {
        let t = &self.tag;
        let mut v: Vec<Person> = (1..=6)
            .map(|n| Person {
                id: n as i64,
                email: format!("a.{}@example.test", self.personal_marker(n)),
                handle: format!("known-handle-{t}-{n}"),
                name: Some(format!("Name {}", self.personal_marker(10 + n))),
                pin: format!("known-pin-{t}-{n}"),
                token: Some(format!("tok-{}", self.secret_marker(n))),
                nick: format!("nick-{n}"),
                score: 10 * n as i64,
                profile: Some(json!({
                    "$ruum": "pseudonym",
                    "id": "p_000000000000",
                    "mail": self.personal_marker(20 + n),
                })),
                note: Some(self.personal_marker(30 + n)),
            })
            .collect();
        v.push(Person {
            id: 7,
            email: format!("a.known-shared-{t}@example.test"),
            handle: format!("known-handle-{t}-7"),
            name: None,
            pin: format!("known-pin-{t}-7"),
            token: None,
            nick: "nick-7".into(),
            score: 70,
            profile: None,
            note: None,
        });
        v
    }

    /// The `INSERT` of one person, as `member`.
    async fn insert_person(&mut self, db: &Db, p: &Person) -> Step<()> {
        let e = self.engine;
        let sql = format!(
            "insert into people (id, email, handle, name, pin, token, nick, score, profile, note) \
             values ({}, {}, {}, {}, {}, {}, {}, {}, {}, {})",
            e.ph(1),
            e.ph(2),
            e.ph(3),
            e.ph(4),
            e.ph(5),
            e.ph(6),
            e.ph(7),
            e.ph(8),
            self.json_param(9),
            e.ph(10)
        );
        let profile = p.profile.as_ref().map(|v| json!(v.to_string()));
        self.write_ok(
            Who::Member,
            db,
            &sql,
            vec![
                json!(p.id),
                json!(p.email),
                json!(p.handle),
                json!(p.name),
                json!(p.pin),
                json!(p.token),
                json!(p.nick),
                json!(p.score),
                profile.unwrap_or(Value::Null),
                json!(p.note),
            ],
        )
        .await
    }

    /// Confirms classes as `member`.
    pub async fn confirm(&mut self, db: &Db, classes: &[(&str, &str, &str)]) -> Step<()> {
        let columns: Vec<Value> = classes
            .iter()
            .map(|(t, c, k)| {
                json!({ "schema": self.engine.schema(), "table": t, "column": c, "class": k })
            })
            .collect();
        let spec = RequestSpec::post(self.path(db, "/columns/confirm"))
            .json(json!({ "columns": columns }));
        let res = self.send(Who::Member, db, spec).await?;
        want(&res, 200, None, "confirmColumnClasses")
    }

    /// Gives `agent_out` a grant of `kind` on `db`, from `member`.
    pub async fn grant(&mut self, db: &Db, kind: &str) -> Step<()> {
        let agent = self.w.a.principal(Who::AgentOut).id;
        let spec = RequestSpec::post(self.path(db, "/access-grants")).json(json!({
            "kind": kind,
            "agent_id": agent,
            "duration_minutes": 60,
            "reason": "privacy suite",
        }));
        let res = self.send(Who::Member, db, spec).await?;
        want(
            &res,
            201,
            None,
            &format!("createDatabaseAccessGrant {kind}"),
        )?;
        if kind == "read_personal" {
            self.read_personal.insert(db.id.clone());
        }
        Ok(())
    }

    /// Makes a branch of `src` as `member` (`masked`, `schema_only` or `full`).
    pub async fn branch(&mut self, src: &Db, data: &str) -> Step<Db> {
        let spec = RequestSpec::post(self.path(src, "/branches"))
            .json(json!({ "name": format!("{} {data} branch", self.case), "data": data }));
        let res = self.send(Who::Member, src, spec).await?;
        want(&res, 202, None, &format!("createDatabaseBranch {data}"))?;
        let id = res
            .json()
            .and_then(|v| v["database"]["node"]["id"].as_str().map(str::to_string))
            .ok_or_else(|| {
                Stop::Failed(format!(
                    "createDatabaseBranch gave no node ID: {}",
                    excerpt(&res)
                ))
            })?;
        let db = Db {
            id,
            data: if data == "full" {
                Data::Real
            } else {
                Data::Masked
            },
        };
        self.wait_ready(&db).await?;
        Ok(db)
    }

    /// Copies `src` into `Private` as `who` (`copyNode`).
    pub async fn copy(&mut self, who: Who, src: &Db, data: Data) -> Step<Db> {
        let spec = RequestSpec::post(format!("/v1/nodes/{}/copy", src.id)).json(json!({
            "parent_id": self.w.a.private,
            "name": format!("{} copy by {}", self.case, who.name()),
        }));
        let res = self.send(who, src, spec).await?;
        want(&res, 201, None, &format!("copyNode as {}", who.name()))?;
        let id = res
            .json()
            .and_then(|v| v["node"]["id"].as_str().map(str::to_string))
            .ok_or_else(|| Stop::Failed(format!("copyNode gave no node ID: {}", excerpt(&res))))?;
        let db = Db { id, data };
        self.wait_ready(&db).await?;
        Ok(db)
    }

    /// The columns of `db` (`listDatabaseColumns`) as `who`.
    pub async fn columns(&mut self, who: Who, db: &Db) -> Step<Vec<Value>> {
        let spec = RequestSpec::get(self.path(db, "/columns?limit=500"));
        let res = self.send(who, db, spec).await?;
        want(&res, 200, None, "listDatabaseColumns")?;
        Ok(res
            .json()
            .and_then(|v| v["items"].as_array().cloned())
            .unwrap_or_default())
    }

    /// The column `table.column` of `listDatabaseColumns`.
    pub async fn column(&mut self, db: &Db, table: &str, column: &str) -> Step<Value> {
        let cols = self.columns(Who::Member, db).await?;
        cols.into_iter()
            .find(|c| c["table"] == table && c["column"] == column)
            .ok_or_else(|| Stop::Failed(format!("listDatabaseColumns has no {table}.{column}")))
    }

    /// Fails unless `table.column` is unconfirmed and counts as `personal`.
    pub async fn expect_unconfirmed(&mut self, db: &Db, table: &str, column: &str) -> Step<()> {
        let c = self.column(db, table, column).await?;
        ensure!(
            c["confirmed_class"].is_null() && c["data_class"] == "personal",
            "{table}.{column} must be unconfirmed (personal) after the migration, got {c}"
        );
        Ok(())
    }

    /// The query log of `db` as `who`.
    pub async fn log(&mut self, who: Who, db: &Db) -> Step<Vec<Value>> {
        let spec = RequestSpec::get(self.path(db, "/queries?limit=500"));
        let res = self.send(who, db, spec).await?;
        want(&res, 200, None, "listDatabaseQueries")?;
        Ok(res
            .json()
            .and_then(|v| v["items"].as_array().cloned())
            .unwrap_or_default())
    }

    /// The pseudonym of `table.column` of the row `id` through the agent's lens.
    pub async fn lens_pseudonym(
        &mut self,
        db: &Db,
        table: &str,
        column: &str,
        id: i64,
    ) -> Step<String> {
        let sql = format!("select {column} from {table} where id = {id}");
        let r = self.rows(Who::AgentOut, db, &sql, vec![]).await?;
        let cell = r.cell(0, 0)?;
        pseudonym_id(&cell)
            .ok_or_else(|| Stop::Failed(format!("{sql} as agent_out gave {cell}, not a pseudonym")))
    }

    /// The end of every case: on Postgres, the stored query log, approval parameters and status
    /// of tenant A hold no marker at all.
    async fn sweep(&mut self) -> Step<()> {
        if self.backend != Kind::Postgres || self.fixture.is_none() {
            return Ok(());
        }
        let text = stored_rows(&self.w.a.id.to_string()).await?;
        let found = leaks_in_text(&text, &self.all_markers());
        ensure!(
            found.is_empty(),
            "stored rows (database_queries, database_approvals.params, typed_nodes status) hold {found:?}"
        );
        Ok(())
    }
}

/// The stored rows of tenant `tenant` that must never hold a raw value, as text. Finds the
/// suite database of the world by the tenant ID (the tenant IDs are unique).
async fn stored_rows(tenant: &str) -> Step<String> {
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| Stop::Failed("the stored-row check needs DATABASE_URL".into()))?;
    let server =
        PgConnectOptions::from_str(&url).map_err(|e| Stop::Failed(format!("DATABASE_URL: {e}")))?;
    let mut admin = server
        .connect()
        .await
        .map_err(|e| Stop::Failed(format!("connect to DATABASE_URL: {e}")))?;
    let names: Vec<String> = sqlx::query_scalar(
        "select datname::text from pg_database where datname like 'ruum\\_suite\\_%' \
         and datname not like '%\\_tpl' order by datname desc",
    )
    .fetch_all(&mut admin)
    .await
    .map_err(|e| Stop::Failed(format!("list the suite databases: {e}")))?;
    let _ = admin.close().await;
    for name in names {
        let Ok(mut conn) = server.clone().database(&name).connect().await else {
            continue;
        };
        // The platform tables have FORCEd row-level security: read as the system.
        let _ = sqlx::query("select set_config('ruum.system', 'on', false)")
            .execute(&mut conn)
            .await;
        let found: bool = sqlx::query_scalar(
            "select to_regclass('database_queries') is not null \
             and exists (select 1 from tenants where id = $1::uuid)",
        )
        .bind(tenant)
        .fetch_one(&mut conn)
        .await
        .unwrap_or(false);
        if !found {
            let _ = conn.close().await;
            continue;
        }
        let text: String = sqlx::query_scalar(
            "select coalesce(string_agg(t, E'\\n'), '') from ( \
               select row_to_json(q)::text as t from database_queries q where tenant_id = $1::uuid \
               union all select params::text from database_approvals where tenant_id = $1::uuid \
               union all select coalesce(state_message, '') || ' ' || state_detail::text \
                 from typed_nodes where tenant_id = $1::uuid \
             ) s",
        )
        .bind(tenant)
        .fetch_one(&mut conn)
        .await
        .map_err(|e| Stop::Failed(format!("read the stored rows in {name}: {e}")))?;
        let _ = conn.close().await;
        return Ok(text);
    }
    fail!("no suite database holds tenant {tenant}; the stored-row check cannot run")
}

// ----------------------------------------------------------------------------- answers

/// The start of an answer, for messages. Never used for an answer that may hold a marker the
/// reader may not see: the reports are logs too, so markers are cut out.
fn excerpt(res: &Response) -> String {
    let body = String::from_utf8_lossy(&res.body);
    let cut: String = body.chars().take(400).collect();
    scrub(&cut)
}

/// Replaces every marker in `s` with `<marker>`, so a failure report holds no raw value.
pub fn scrub(s: &str) -> String {
    let mut s = s.to_string();
    while let Some(i) = s.find("ruum-raw-") {
        let end = s[i..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .map(|e| i + e)
            .unwrap_or(s.len());
        s.replace_range(i..end, "<marker>");
    }
    s
}

/// Fails unless the answer has `status` (and `error.code`, when given).
pub fn want(res: &Response, status: u16, code: Option<&str>, what: &str) -> Step<()> {
    let ok = res.status.as_u16() == status
        && code.is_none_or(|c| res.error_code().as_deref() == Some(c));
    ensure!(
        ok,
        "{what}: want {status}{}, got {} {}",
        code.map(|c| format!(" {c}")).unwrap_or_default(),
        res.status,
        excerpt(res)
    );
    Ok(())
}

/// Fails unless the answer is one of `allowed` (status, optional `error.code`).
pub fn want_any(res: &Response, allowed: &[(u16, Option<&str>)], what: &str) -> Step<()> {
    let ok = allowed.iter().any(|(s, c)| {
        res.status.as_u16() == *s && c.is_none_or(|c| res.error_code().as_deref() == Some(c))
    });
    ensure!(
        ok,
        "{what}: want one of {allowed:?}, got {} {}",
        res.status,
        excerpt(res)
    );
    Ok(())
}

/// A refusal of a statement: `422 statement_not_allowed`, or the engine's own refusal
/// (`400 query_failed`), or `403` of the privacy rules.
const REFUSED: &[(u16, Option<&str>)] = &[
    (422, Some("statement_not_allowed")),
    (400, Some("query_failed")),
    (403, None),
];

/// The rows of a `QueryResult`.
#[derive(Clone, Debug)]
pub struct Rows {
    pub value: Value,
}

impl Rows {
    pub fn parse(res: &Response) -> Step<Rows> {
        let value = res.json().ok_or_else(|| {
            Stop::Failed(format!("the query answer is not JSON: {}", excerpt(res)))
        })?;
        ensure!(
            value["rows"].is_array() && value["columns"].is_array(),
            "the query answer is not a QueryResult: {}",
            excerpt(res)
        );
        ensure!(
            value["content_trust"] == "untrusted",
            "a QueryResult must say content_trust: untrusted"
        );
        Ok(Rows { value })
    }

    pub fn rows(&self) -> Vec<Vec<Value>> {
        self.value["rows"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| r.as_array().cloned().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.rows().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cell(&self, r: usize, c: usize) -> Step<Value> {
        self.rows()
            .get(r)
            .and_then(|row| row.get(c).cloned())
            .ok_or_else(|| Stop::Failed(format!("the result has no cell ({r}, {c})")))
    }

    /// A cell as an integer (numbers may come as strings).
    pub fn int(&self, r: usize, c: usize) -> Step<i64> {
        let v = self.cell(r, c)?;
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            .ok_or_else(|| Stop::Failed(format!("cell ({r}, {c}) is {v}, not an integer")))
    }

    /// The column metadata (`ResultColumn`) of column `c`.
    pub fn column(&self, c: usize) -> Value {
        self.value["columns"][c].clone()
    }

    /// One column of every row.
    pub fn col(&self, c: usize) -> Vec<Value> {
        self.rows()
            .into_iter()
            .map(|r| r.get(c).cloned().unwrap_or(Value::Null))
            .collect()
    }
}

/// Fails unless cell `(r, c)` is `want`, or masked. An aggregate or a function over a restricted
/// column has that column's class, so the server may mask its result; it must never be the value
/// that the real data would give.
fn int_or_masked(rows: &Rows, r: usize, c: usize, want: i64, what: &str) -> Step<()> {
    let v = rows.cell(r, c)?;
    if is_masked(&v) {
        return Ok(());
    }
    let n = rows.int(r, c)?;
    ensure!(n == want, "{what}: want {want} or a masked value, got {n}");
    Ok(())
}

/// Fails unless column `c` of `r` has the class and the masking.
fn want_column(r: &Rows, c: usize, class: &str, masking: &str, what: &str) -> Step<()> {
    let col = r.column(c);
    ensure!(
        col["data_class"] == class && col["masking"] == masking,
        "{what}: column {c} must be {class}/{masking}, got {col}"
    );
    if masking == "pseudonym" {
        ensure!(
            col["type"] == "text",
            "{what}: a masked personal column has the type text, got {col}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------------- bodies

mod cases {
    use super::*;

    type F<'a> = CaseFuture<'a>;

    const A: Who = Who::AgentOut;
    const M: Who = Who::Member;
    const V: Who = Who::MemberView;
    const E: Who = Who::MemberEdit;

    pub fn query_plain_column(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select id, email, name, token, score, note from people order by id",
                    vec![],
                )
                .await?;
            ensure!(r.len() == f.people.len(), "the agent must see every row");
            want_column(&r, 0, "normal", "none", "id")?;
            want_column(&r, 1, "personal", "pseudonym", "email")?;
            want_column(&r, 2, "personal", "pseudonym", "name")?;
            want_column(&r, 3, "secret", "hidden", "token")?;
            want_column(&r, 4, "normal", "none", "score")?;
            want_column(&r, 5, "personal", "pseudonym", "note (unconfirmed)")?;
            for (i, p) in f.people.iter().enumerate() {
                ensure!(
                    pseudonym_id(&r.cell(i, 1)?).is_some(),
                    "email of row {i} is not a pseudonym"
                );
                let token = r.cell(i, 3)?;
                match p.token {
                    Some(_) => ensure!(is_hidden(&token, "secret"), "token of row {i} is {token}"),
                    None => ensure!(token.is_null(), "a NULL secret reads as NULL, got {token}"),
                }
                ensure!(r.int(i, 4)? == p.score, "score is normal and shown as is");
            }
            // The person control: the real values are there.
            let rm = c
                .rows(M, &f.db, "select email from people where id = 1", vec![])
                .await?;
            ensure!(
                rm.cell(0, 0)? == json!(f.people[0].email),
                "member must read the real email"
            );
            Ok(())
        })
    }

    pub fn query_expression(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select lower(email), email || '', coalesce(name, 'x'), upper(token), score + 1, \
                     'constant' from people where id = 1",
                    vec![],
                )
                .await?;
            for i in 0..3 {
                let col = r.column(i);
                ensure!(
                    col["data_class"] == "personal",
                    "expression {i} reads a personal column: {col}"
                );
                ensure!(is_masked(&r.cell(0, i)?), "expression {i} must be masked");
            }
            ensure!(
                r.column(3)["data_class"] == "secret",
                "upper(token) is secret"
            );
            let t = r.cell(0, 3)?;
            ensure!(
                is_hidden(&t, "secret") || t.is_null(),
                "upper(token) must be hidden, got {t}"
            );
            want_column(&r, 4, "normal", "none", "score + 1")?;
            want_column(&r, 5, "normal", "none", "a constant")?;
            ensure!(
                r.cell(0, 5)? == json!("constant"),
                "a constant comes back as is"
            );
            Ok(())
        })
    }

    pub fn query_join(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let n = c
                .count(
                    A,
                    &f.db,
                    "select count(*) from orders o join people p on p.email = o.person_email",
                    vec![],
                )
                .await?;
            ensure!(
                n == 6,
                "a join of two personal columns works on pseudonyms: want 6, got {n}"
            );
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select p.email, o.person_email from orders o join people p on p.email = o.person_email \
                     order by o.id",
                    vec![],
                )
                .await?;
            for row in r.rows() {
                let (a, b) = (pseudonym_id(&row[0]), pseudonym_id(&row[1]));
                ensure!(
                    a.is_some() && a == b,
                    "both sides read the same pseudonym: {row:?}"
                );
            }
            Ok(())
        })
    }

    pub fn query_aggregate(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let r = c
                .rows(A, &f.db, "select count(distinct email) from people", vec![])
                .await?;
            let all = f.people.len() as i64;
            int_or_masked(&r, 0, 0, all, "count(distinct email) works on pseudonyms")?;
            let agg = match c.engine {
                Engine::Postgres => "string_agg(email, ',')",
                Engine::Sqlite => "group_concat(email, ',')",
            };
            let r = c
                .rows(
                    A,
                    &f.db,
                    &format!("select max(name), {agg}, count(token) from people"),
                    vec![],
                )
                .await?;
            ensure!(is_masked(&r.cell(0, 0)?), "max(name) must be masked");
            ensure!(
                r.column(1)["data_class"] == "personal",
                "an aggregate of emails is personal"
            );
            int_or_masked(&r, 0, 2, 6, "count(token) counts the non-NULL secrets")?;
            Ok(())
        })
    }

    pub fn query_jsonb_ruum(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            // A person sees the real object, wrapped as a literal.
            let r = c.rows(M, &f.db, "select profile, meta from people p join orders o on o.id = p.id where p.id = 1", vec![]).await?;
            let p = r.cell(0, 0)?;
            ensure!(
                p["$ruum"] == "literal" && p["value"]["$ruum"] == "pseudonym",
                "an object with $ruum comes back as a literal: {}",
                p["$ruum"]
            );
            // The agent: the personal object is a pseudonym, the normal one a literal, never a
            // bare marker.
            let r = c.rows(A, &f.db, "select profile, meta from people p join orders o on o.id = p.id where p.id = 1", vec![]).await?;
            ensure!(
                pseudonym_id(&r.cell(0, 0)?).is_some(),
                "a personal jsonb is a pseudonym for the agent"
            );
            let m = r.cell(0, 1)?;
            ensure!(
                m["$ruum"] == "literal"
                    && m["value"] == json!({ "$ruum": "hidden", "class": "secret" }),
                "a normal jsonb with $ruum is a literal, got {m}"
            );
            // A path into the masked object fails or stays masked; no value either way.
            let res = c
                .query(
                    A,
                    &f.db,
                    "select profile->>'mail' from people where id = 1",
                    vec![],
                    false,
                )
                .await?;
            if res.status.as_u16() == 200 {
                let r = Rows::parse(&res)?;
                let v = r.cell(0, 0)?;
                ensure!(
                    v.is_null() || is_masked(&v),
                    "profile->>'mail' must be masked, got {v}"
                );
            } else {
                want_any(&res, REFUSED, "profile->>'mail'")?;
            }
            Ok(())
        })
    }

    pub fn query_same_value(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let a = c.lens_pseudonym(&f.db, "people", "email", 1).await?;
            let b = c.lens_pseudonym(&f.db, "orders", "person_email", 1).await?;
            ensure!(
                a == b,
                "the same value in two columns gives the same pseudonym ({a} != {b})"
            );
            let again = c.lens_pseudonym(&f.db, "people", "email", 1).await?;
            ensure!(
                a == again,
                "the same value gives the same pseudonym in two queries"
            );
            let other = c.lens_pseudonym(&f.db, "people", "email", 2).await?;
            ensure!(a != other, "two values give two pseudonyms");
            Ok(())
        })
    }

    pub fn query_unconfirmed(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let spec = RequestSpec::get(c.path(&f.db, "/schema"));
            let res = c.send(A, &f.db, spec).await?;
            want(&res, 200, None, "getDatabaseSchema")?;
            let v = res.json().unwrap_or_default();
            let note = v["tables"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["name"] == "people")
                .flat_map(|t| t["columns"].as_array().cloned().unwrap_or_default())
                .find(|col| col["name"] == "note")
                .ok_or_else(|| Stop::Failed("getDatabaseSchema has no people.note".into()))?;
            ensure!(
                note["data_class"] == "personal" && note["confirmed"] == false,
                "people.note is unconfirmed and counts as personal, got {note}"
            );
            c.expect_unconfirmed(&f.db, "people", "note").await?;
            let r = c
                .rows(A, &f.db, "select note from people where id = 1", vec![])
                .await?;
            ensure!(
                pseudonym_id(&r.cell(0, 0)?).is_some(),
                "an unconfirmed column reads as a pseudonym"
            );
            let n = c
                .count(
                    A,
                    &f.db,
                    "select count(*) from people where note like 'ruum%'",
                    vec![],
                )
                .await?;
            ensure!(
                n == 0,
                "a filter on an unconfirmed column compares the pseudonym, got {n}"
            );
            Ok(())
        })
    }

    pub fn query_secret_without_manage(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            for who in [V, E] {
                let r = c
                    .rows(
                        who,
                        &f.db,
                        "select token, pin, email from people where id = 1",
                        vec![],
                    )
                    .await?;
                ensure!(
                    is_hidden(&r.cell(0, 0)?, "secret"),
                    "{} reads token hidden",
                    who.name()
                );
                ensure!(
                    is_hidden(&r.cell(0, 1)?, "secret"),
                    "{} reads pin hidden",
                    who.name()
                );
                ensure!(
                    r.cell(0, 2)? == json!(f.people[0].email),
                    "{} reads personal values",
                    who.name()
                );
                let n = c
                    .count(
                        who,
                        &f.db,
                        "select count(*) from people where pin like 'known%'",
                        vec![],
                    )
                    .await?;
                ensure!(
                    n == 0,
                    "{}: a filter on a secret sees no value, got {n}",
                    who.name()
                );
            }
            let n = c
                .count(
                    M,
                    &f.db,
                    "select count(*) from people where pin like 'known%'",
                    vec![],
                )
                .await?;
            ensure!(
                n == f.people.len() as i64,
                "the manager's control finds every pin, got {n}"
            );
            Ok(())
        })
    }

    pub fn query_read_personal(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            c.grant(&f.db, "read_personal").await?;
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select email, token, pin from people where id = 1",
                    vec![],
                )
                .await?;
            ensure!(
                r.cell(0, 0)? == json!(f.people[0].email),
                "read_personal shows the real email"
            );
            ensure!(
                is_hidden(&r.cell(0, 1)?, "secret"),
                "read_personal never shows a secret"
            );
            ensure!(
                is_hidden(&r.cell(0, 2)?, "secret"),
                "read_personal never shows a secret"
            );
            let n = c
                .count(
                    A,
                    &f.db,
                    "select count(*) from people where pin like 'known%'",
                    vec![],
                )
                .await?;
            ensure!(
                n == 0,
                "read_personal: a filter on a secret sees no value, got {n}"
            );
            Ok(())
        })
    }

    pub fn schema_names_only(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            for who in [A, V, M] {
                // Names only: no value of any class, also for the manager.
                let all = c.all_markers();
                let spec = RequestSpec::get(c.path(&f.db, "/schema"));
                let res = c.send_checked(who, spec, &all).await?;
                want(&res, 200, None, "getDatabaseSchema")?;
                let text = res.text();
                ensure!(
                    !text.contains("known-pin-") && !text.contains("known-handle-"),
                    "the schema holds a value"
                );
            }
            Ok(())
        })
    }

    pub fn queries_agent_log(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let sql = format!("select id from people where handle = {}", c.engine.ph(1));
            let handle = f.people[0].handle.clone();
            let n = c.rows(A, &f.db, &sql, vec![json!(handle)]).await?.len();
            ensure!(
                n == 0,
                "the agent's filter on a personal column compares the pseudonym"
            );
            let entries = c.log(A, &f.db).await?;
            let agent = c.w.a.principal(A).id.to_string();
            ensure!(!entries.is_empty(), "the agent sees its own entry");
            for e in &entries {
                ensure!(
                    e["by"]["actor"]["id"] == agent.as_str(),
                    "an agent sees only its own entries: {}",
                    e["by"]
                );
                for p in e["params"].as_array().into_iter().flatten() {
                    ensure!(
                        p.is_null() || is_hidden(p, "personal"),
                        "a parameter is a hidden marker, got {p}"
                    );
                }
            }
            let text = entries
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(
                !text.contains(&handle),
                "the log holds the parameter's value"
            );
            Ok(())
        })
    }

    /// The approval of an agent's `UPDATE` without `WHERE` with a parameter: its ID and its one
    /// parameter as the agent reads it.
    async fn approval_of(c: &mut Ctx, f: &Fixture, value: &str) -> Step<(String, Value)> {
        c.grant(&f.db, "write_live").await?;
        let sql = format!("update orders set label = {}", c.engine.ph(1));
        let res = c.query(A, &f.db, &sql, vec![json!(value)], true).await?;
        want(&res, 202, None, "an agent's UPDATE without WHERE")?;
        let id = res
            .json()
            .and_then(|v| v["approval"]["id"].as_str().map(str::to_string))
            .ok_or_else(|| Stop::Failed("the 202 has no approval ID".into()))?;
        let param = approval_param_as(c, f, &id, A, value).await?;
        Ok((id, param))
    }

    /// Reads approval `id` as `who` and checks that its one parameter is the hidden marker
    /// (decision 0004-24: for every reader) and that the answer does not hold `value`. The
    /// explicit check is needed: `value` may have no marker (for example `handle`).
    async fn approval_param_as(
        c: &mut Ctx,
        f: &Fixture,
        id: &str,
        who: Who,
        value: &str,
    ) -> Step<Value> {
        let spec = RequestSpec::get(c.path(&f.db, &format!("/approvals/{id}")));
        let res = c.send(who, &f.db, spec).await?;
        want(
            &res,
            200,
            None,
            &format!("getDatabaseApproval as {}", who.name()),
        )?;
        let a = res.json().unwrap_or_default();
        let params = a["params"].as_array().cloned().unwrap_or_default();
        ensure!(
            params.len() == 1,
            "the approval has one parameter for {}, got {}",
            who.name(),
            a["params"]
        );
        ensure!(
            is_hidden(&params[0], "personal"),
            "an approval parameter is a hidden marker for {}, got {}",
            who.name(),
            params[0]
        );
        ensure!(
            !res.text().contains(value),
            "the approval holds the parameter's value for {}",
            who.name()
        );
        Ok(params[0].clone())
    }

    pub fn approval_params_hidden(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let value = f.people[1].handle.clone();
            let (id, _) = approval_of(c, &f, &value).await?;
            // The same approval holds no value for its manager (manage) and for a person with
            // view only either: the stored parameter is a marker for every reader.
            for who in [M, V] {
                approval_param_as(c, &f, &id, who, &value).await?;
            }
            Ok(())
        })
    }

    pub fn branch_masked(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let b = c.branch(&f.db, "masked").await?;
            // Db::data is Masked: no marker for anyone, also for the manager.
            for who in [M, A, V] {
                let r = c
                    .rows(
                        who,
                        &b,
                        "select email, token, pin, handle from people order by id",
                        vec![],
                    )
                    .await?;
                ensure!(r.len() == f.people.len(), "the masked branch has the rows");
                for row in r.rows() {
                    ensure!(
                        row[1].is_null() || is_hidden(&row[1], "secret"),
                        "a masked secret is NULL: {}",
                        row[1]
                    );
                    ensure!(
                        row[2].is_null() || is_hidden(&row[2], "secret"),
                        "a masked secret is NULL: {}",
                        row[2]
                    );
                }
            }
            let rm = c
                .rows(M, &b, "select handle from people where id = 1", vec![])
                .await?;
            let h = rm.cell(0, 0)?;
            ensure!(
                h != json!(f.people[0].handle),
                "the masked branch holds the real handle"
            );
            let n = c
                .count(
                    M,
                    &b,
                    "select count(*) from people p join orders o on o.handle_copy = p.handle",
                    vec![],
                )
                .await?;
            ensure!(
                n == 0,
                "the masked branch holds real personal values (join found {n})"
            );
            Ok(())
        })
    }

    pub fn copy_agent(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let copy = c.copy(A, &f.db, Data::Masked).await?;
            let n = c
                .count(
                    M,
                    &copy,
                    "select count(*) from people p join orders o on o.handle_copy = p.handle",
                    vec![],
                )
                .await?;
            ensure!(
                n == 0,
                "the agent's copy holds real personal values (join found {n})"
            );
            let r = c
                .rows(
                    M,
                    &copy,
                    "select token, pin from people where id = 1",
                    vec![],
                )
                .await?;
            ensure!(
                r.cell(0, 0)?.is_null() && r.cell(0, 1)?.is_null(),
                "a masked copy holds no secret"
            );
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ oracles

    pub fn oracle_like_count(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let all = f.people.len() as i64;
            let real = c
                .count(
                    M,
                    &f.db,
                    "select count(*) from people where email like 'a%'",
                    vec![],
                )
                .await?;
            ensure!(
                real == all,
                "control: every real email starts with 'a', got {real}"
            );
            let n = c
                .count(
                    A,
                    &f.db,
                    "select count(*) from people where email like 'a%'",
                    vec![],
                )
                .await?;
            ensure!(n == 0, "the agent's LIKE compares the real value: {n} rows");
            let n = c
                .count(
                    A,
                    &f.db,
                    "select count(*) from people where email like 'p%'",
                    vec![],
                )
                .await?;
            ensure!(
                n == all,
                "the agent's LIKE compares the pseudonym: want {all}, got {n}"
            );
            for who in [A, V] {
                let n = c
                    .count(
                        who,
                        &f.db,
                        "select count(*) from people where pin like 'k%'",
                        vec![],
                    )
                    .await?;
                ensure!(
                    n == 0,
                    "{}: LIKE on a secret sees the value: {n} rows",
                    who.name()
                );
            }
            Ok(())
        })
    }

    pub fn oracle_eq_param(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let sql = format!(
                "select count(*) from people where email = {}",
                c.engine.ph(1)
            );
            let value = json!(f.shared().email);
            let real = c.count(M, &f.db, &sql, vec![value.clone()]).await?;
            ensure!(real == 1, "control: the person finds the row, got {real}");
            let n = c.count(A, &f.db, &sql, vec![value]).await?;
            ensure!(n == 0, "the agent's = $1 with the real value finds the row");
            let sql = format!("select count(*) from people where pin = {}", c.engine.ph(1));
            let pin = json!(f.people[0].pin);
            for who in [A, V] {
                let n = c.count(who, &f.db, &sql, vec![pin.clone()]).await?;
                ensure!(n == 0, "{}: = $1 on a secret finds the row", who.name());
            }
            Ok(())
        })
    }

    pub fn oracle_order_by(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select id, email from people order by email",
                    vec![],
                )
                .await?;
            let ids: Vec<String> = r.col(1).iter().filter_map(pseudonym_id).collect();
            ensure!(ids.len() == f.people.len(), "every email is a pseudonym");
            let mut sorted = ids.clone();
            sorted.sort();
            ensure!(ids == sorted, "ORDER BY email must order by the pseudonym");
            let r = c
                .rows(A, &f.db, "select id from people order by handle", vec![])
                .await?;
            let by_handle: Vec<i64> = (0..r.len()).filter_map(|i| r.int(i, 0).ok()).collect();
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select id, handle from people order by handle",
                    vec![],
                )
                .await?;
            let mut pairs: Vec<(String, i64)> = (0..r.len())
                .filter_map(|i| Some((pseudonym_id(&r.cell(i, 1).ok()?)?, r.int(i, 0).ok()?)))
                .collect();
            pairs.sort();
            let want: Vec<i64> = pairs.into_iter().map(|p| p.1).collect();
            ensure!(
                by_handle == want,
                "ORDER BY handle without the column in the result orders by the pseudonym"
            );
            Ok(())
        })
    }

    pub fn oracle_group_by(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select email, count(*) from people group by email",
                    vec![],
                )
                .await?;
            ensure!(r.len() == f.people.len(), "one group per email");
            for row in r.rows() {
                ensure!(
                    pseudonym_id(&row[0]).is_some(),
                    "a group key is a pseudonym"
                );
            }
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select substr(email, 1, 1), count(*) from people group by substr(email, 1, 1)",
                    vec![],
                )
                .await?;
            for row in r.rows() {
                ensure!(
                    row[0] != json!("a"),
                    "GROUP BY an expression over email sees the real value"
                );
            }
            Ok(())
        })
    }

    pub fn oracle_min(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let all = c.rows(A, &f.db, "select email from people", vec![]).await?;
            let min = all.col(0).iter().filter_map(pseudonym_id).min();
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select min(email), max(email) from people",
                    vec![],
                )
                .await?;
            let got = pseudonym_id(&r.cell(0, 0)?);
            ensure!(
                got.is_some() && got == min,
                "min(email) is the smallest pseudonym"
            );
            Ok(())
        })
    }

    pub fn oracle_length(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let len = shape_example().len() as i64;
            let real = c
                .rows(M, &f.db, "select length(email) from people", vec![])
                .await?;
            ensure!(
                (0..real.len()).all(|i| real.int(i, 0).ok() != Some(len)),
                "control: real lengths differ from {len}"
            );
            let r = c.rows(A, &f.db, "select length(email), length(name), length(note) from people where name is not null", vec![]).await?;
            for i in 0..r.len() {
                for col in 0..3 {
                    int_or_masked(&r, i, col, len, "length() of a personal column")?;
                }
            }
            for who in [A, V] {
                let r = c
                    .rows(who, &f.db, "select length(pin) from people", vec![])
                    .await?;
                for v in r.col(0) {
                    ensure!(
                        v.is_null() || is_hidden(&v, "secret"),
                        "{}: length(pin) shows {v}",
                        who.name()
                    );
                }
            }
            Ok(())
        })
    }

    pub fn oracle_join_personal_normal(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let sql = "select count(*) from people p join orders o on o.handle_copy = p.handle";
            let real = c.count(M, &f.db, sql, vec![]).await?;
            ensure!(
                real == 6,
                "control: the person's join finds 6 rows, got {real}"
            );
            let n = c.count(A, &f.db, sql, vec![]).await?;
            ensure!(
                n == 0,
                "the agent's join of a personal and a normal column sees the value: {n} rows"
            );
            let n = c.count(A, &f.db, "select count(*) from people p where exists (select 1 from orders o where o.handle_copy = p.handle)", vec![]).await?;
            ensure!(n == 0, "the same through a subquery: {n} rows");
            Ok(())
        })
    }

    pub fn oracle_join_secret_normal(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let sql = "select count(*) from people p join orders o on o.pin_copy = p.pin";
            let real = c.count(M, &f.db, sql, vec![]).await?;
            ensure!(
                real == 6,
                "control: the manager's join finds 6 rows, got {real}"
            );
            for who in [A, V, E] {
                let n = c.count(who, &f.db, sql, vec![]).await?;
                ensure!(
                    n == 0,
                    "{}: a join of a secret and a normal column sees the value: {n}",
                    who.name()
                );
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ writes

    /// Runs an agent write on live (with `write_live`) and on a masked branch: both `403
    /// class_read_required`; then the manager sees nothing changed in the normal column.
    async fn class_read_required(c: &mut Ctx, sql: &str, check: &str, want_check: i64) -> Step<()> {
        let f = c.fixture().await?;
        c.grant(&f.db, "write_live").await?;
        let b = c.branch(&f.db, "masked").await?;
        for db in [f.db.clone(), b.clone()] {
            let res = c.query(A, &db, sql, vec![], true).await?;
            want(
                &res,
                403,
                Some("class_read_required"),
                &format!("{sql} on {:?} data", db.data),
            )?;
            let n = c.count(M, &db, check, vec![]).await?;
            ensure!(
                n == want_check,
                "after the refused write, {check} gives {n}, want {want_check}"
            );
        }
        Ok(())
    }

    pub fn write_update_set(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            class_read_required(
                c,
                "update people set nick = email where id = 1",
                "select count(*) from people where nick = 'nick-1'",
                1,
            )
            .await
        })
    }

    pub fn write_insert_select(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            class_read_required(
                c,
                "insert into orders (id, person_email, handle_copy, pin_copy, amount, label) \
                 select id + 100, 'x', 'x', 'x', 0, email from people",
                "select count(*) from orders",
                6,
            )
            .await
        })
    }

    pub fn write_returning(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            class_read_required(
                c,
                "update people set score = score + 1 where id = 1 returning email",
                "select count(*) from people where id = 1 and score = 10",
                1,
            )
            .await
        })
    }

    // ------------------------------------------------------------------------ renames

    pub fn rename_onto_dropped_normal(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            c.migrate_ok(&f.db, "alter table people drop column nick;\nalter table people rename column email to nick;").await?;
            c.expect_unconfirmed(&f.db, "people", "nick").await?;
            let r = c
                .rows(A, &f.db, "select nick from people where id = 1", vec![])
                .await?;
            ensure!(
                pseudonym_id(&r.cell(0, 0)?).is_some(),
                "the renamed column reads as a pseudonym"
            );
            Ok(())
        })
    }

    pub fn rename_table_onto_dropped(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            c.migrate_ok(
                &f.db,
                "drop table orders;\nalter table people rename to orders;",
            )
            .await?;
            let cols: Vec<Value> = c
                .columns(M, &f.db)
                .await?
                .into_iter()
                .filter(|col| col["table"] == "orders")
                .collect();
            ensure!(!cols.is_empty(), "the renamed table has columns");
            for col in &cols {
                ensure!(
                    col["confirmed_class"].is_null(),
                    "orders.{} must be unconfirmed after the swap",
                    col["column"]
                );
            }
            // Every column of the swapped table is unconfirmed, `id` too, so the agent reads
            // `id` as a pseudonym (text): a filter `id = 1` cannot find the row (Postgres:
            // no `text = integer` operator, a refusal; SQLite: no row). So the case reads the
            // first row by order instead, and checks the filter apart.
            let res = c
                .query(
                    A,
                    &f.db,
                    "select id from orders where id = 1",
                    vec![],
                    false,
                )
                .await?;
            if res.status.as_u16() == 200 {
                ensure!(
                    Rows::parse(&res)?.is_empty(),
                    "a filter on the masked id finds the row"
                );
            } else {
                want_any(&res, REFUSED, "a filter on the masked id")?;
            }
            let r = c
                .rows(
                    A,
                    &f.db,
                    "select id, email, nick, score from orders order by id limit 1",
                    vec![],
                )
                .await?;
            for i in 0..4 {
                ensure!(
                    is_masked(&r.cell(0, i)?),
                    "column {i} of the swapped table must be masked"
                );
            }
            Ok(())
        })
    }

    pub fn rename_retype(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let up = match c.engine {
                Engine::Postgres => {
                    "alter table people alter column score type bigint;".to_string()
                }
                Engine::Sqlite => format!(
                    "{}\ninsert into people_new select * from people;\ndrop table people;\n\
                     alter table people_new rename to people;",
                    fixture_ddl(Engine::Sqlite)
                        .split(";\n")
                        .next()
                        .unwrap_or_default()
                        .replace("create table people (", "create table people_new (")
                        .replace("score integer not null", "score bigint not null")
                        + ";"
                ),
            };
            c.migrate_ok(&f.db, &up).await?;
            c.expect_unconfirmed(&f.db, "people", "score").await?;
            let r = c
                .rows(A, &f.db, "select score from people where id = 1", vec![])
                .await?;
            ensure!(is_masked(&r.cell(0, 0)?), "the retyped column reads masked");
            Ok(())
        })
    }

    pub fn rename_drop_add(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            c.migrate_ok(
                &f.db,
                "alter table people drop column nick;\nalter table people add column nick text;\n\
                 update people set nick = email;",
            )
            .await?;
            c.expect_unconfirmed(&f.db, "people", "nick").await?;
            let r = c
                .rows(A, &f.db, "select nick from people where id = 1", vec![])
                .await?;
            ensure!(
                pseudonym_id(&r.cell(0, 0)?).is_some(),
                "the new column reads as a pseudonym"
            );
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ roles

    /// The agent's `current_user`.
    async fn current_user(c: &mut Ctx, db: &Db) -> Step<Value> {
        let r = c.rows(A, db, "select current_user", vec![]).await?;
        r.cell(0, 0)
    }

    /// Runs a role statement as the agent: refused, or no change of `current_user`; then the
    /// agent still reads masked values.
    async fn role_statement(c: &mut Ctx, sqls: &[String]) -> Step<()> {
        let f = c.fixture().await?;
        let before = current_user(c, &f.db).await?;
        for sql in sqls {
            for write in [false, true] {
                let res = c.query(A, &f.db, sql, vec![], write).await?;
                if res.status.as_u16() == 200 {
                    // The role change and its effect can be in the same statement
                    // (review-T-2 N-3): the answer itself must name no other role and hold
                    // no value without a marker.
                    let r = Rows::parse(&res)?;
                    if let Some(role) = other_role_in(&r, &before) {
                        fail!("{sql}: the answer names the role {role}, not {before}");
                    }
                    let text = res.text();
                    ensure!(
                        !text.contains("known-pin-") && !text.contains("known-handle-"),
                        "{sql}: the answer holds a value"
                    );
                } else {
                    want_any(&res, REFUSED, sql)?;
                }
                let now = current_user(c, &f.db).await?;
                ensure!(
                    now == before,
                    "{sql}: current_user changed from {before} to {now}"
                );
                let r = c
                    .rows(
                        A,
                        &f.db,
                        "select email, token from people where id = 1",
                        vec![],
                    )
                    .await?;
                ensure!(
                    pseudonym_id(&r.cell(0, 0)?).is_some(),
                    "{sql}: the agent reads email unmasked"
                );
                ensure!(
                    is_hidden(&r.cell(0, 1)?, "secret"),
                    "{sql}: the agent reads token unmasked"
                );
                // The base table of another role stays out of reach.
                let res = c
                    .query(A, &f.db, "select email from public.people", vec![], false)
                    .await?;
                if res.status.as_u16() == 200 {
                    let r = Rows::parse(&res)?;
                    ensure!(
                        r.col(0).iter().all(is_masked),
                        "{sql}: public.people shows values"
                    );
                }
            }
        }
        Ok(())
    }

    /// A cell of `r` that names a role other than `before`: `ruum_dbhost` or a login role
    /// (`r_` + hex).
    pub(super) fn other_role_in(r: &Rows, before: &Value) -> Option<String> {
        r.rows().into_iter().flatten().find_map(|cell| {
            let s = cell.as_str()?;
            let login = s
                .strip_prefix("r_")
                .is_some_and(|h| !h.is_empty() && h.chars().all(|ch| ch.is_ascii_hexdigit()));
            ((s == "ruum_dbhost" || login) && before.as_str() != Some(s)).then(|| s.to_string())
        })
    }

    /// The other login roles of the database (`r_` + hex), as the agent can list them.
    async fn other_roles(c: &mut Ctx) -> Step<Vec<String>> {
        let f = c.fixture().await?;
        let sql = "select rolname::text from pg_roles where rolname like 'r\\_%' \
                   and rolname <> current_user order by 1 limit 8";
        let res = c.query(A, &f.db, sql, vec![], false).await?;
        if res.status.as_u16() != 200 {
            // The agent cannot list the roles: then it cannot name them either.
            want_any(&res, REFUSED, "the list of roles")?;
            return Ok(Vec::new());
        }
        let r = Rows::parse(&res)?;
        Ok(r.col(0)
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect())
    }

    pub fn role_reset_role(c: &mut Ctx) -> F<'_> {
        Box::pin(
            async move { role_statement(c, &["reset role".into(), "set role none".into()]).await },
        )
    }

    pub fn role_set_role_owner(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let mut sqls: Vec<String> = other_roles(c)
                .await?
                .iter()
                .map(|r| format!("set role \"{r}\""))
                .collect();
            sqls.push("set session authorization default".into());
            role_statement(c, &sqls).await
        })
    }

    pub fn role_set_role_dbhost(c: &mut Ctx) -> F<'_> {
        Box::pin(async move { role_statement(c, &["set role ruum_dbhost".into()]).await })
    }

    pub fn role_set_config(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let mut sqls = vec![
                "select set_config('role', 'ruum_dbhost', false)".to_string(),
                "select set_config('role', 'ruum_dbhost', true), current_user".to_string(),
                "select set_config('session_authorization', 'ruum_dbhost', false)".to_string(),
                // The role change and a read of the base table in one statement (N-3).
                "select set_config('role', 'ruum_dbhost', true), \
                 query_to_xml('select * from people', true, false, '')::text"
                    .to_string(),
            ];
            for r in other_roles(c).await? {
                sqls.push(format!("select set_config('role', '{r}', false)"));
            }
            role_statement(c, &sqls).await
        })
    }

    pub fn role_do_block(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let mut sqls = vec![
                "do $$ begin execute 'set role ruum_dbhost'; end $$".to_string(),
                "do $$ begin perform set_config('role', 'ruum_dbhost', false); end $$".to_string(),
                "do $$ begin execute 'reset role'; end $$".to_string(),
            ];
            for r in other_roles(c).await? {
                sqls.push(format!("do $$ begin execute 'set role \"{r}\"'; end $$"));
            }
            role_statement(c, &sqls).await
        })
    }

    // ------------------------------------------------------------------------ files

    /// Runs a statement as the agent and expects a refusal (both modes).
    async fn refused(c: &mut Ctx, db: &Db, sql: &str) -> Step<()> {
        for write in [false, true] {
            let res = c.query(A, db, sql, vec![], write).await?;
            want_any(&res, REFUSED, sql)?;
        }
        Ok(())
    }

    pub fn file_attach_other_tenant(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            // The path of a file of tenant B: its manager may read it with the read-only
            // pragma function; otherwise the case cannot name a real file.
            let b = create_database_b(c).await?;
            let path = sqlite_path_b(c, &b).await?;
            refused(c, &f.db, &format!("attach database '{path}' as b")).await?;
            refused(c, &f.db, &format!("attach '{path}' as b2")).await?;
            let res = c
                .query(
                    A,
                    &f.db,
                    "select count(*) from b.sqlite_master",
                    vec![],
                    false,
                )
                .await?;
            want_any(&res, REFUSED, "a read of the attached schema")?;
            Ok(())
        })
    }

    /// A SQLite database in tenant B's `Private`, made by B's member.
    async fn create_database_b(c: &mut Ctx) -> Step<String> {
        let spec = RequestSpec::post("/v1/databases")
            .tenant(c.w.b.id)
            .json(json!({
                "parent_id": c.w.b.private,
                "name": "privacy tenant b",
                "manifest": Engine::Sqlite.manifest(),
            }));
        let member_b = c.w.b.principal(M).clone();
        let res = client::send(&c.app, &spec, &member_b).await;
        want(&res, 202, None, "createDatabase in tenant B")?;
        res.json()
            .and_then(|v| v["database"]["node"]["id"].as_str().map(str::to_string))
            .ok_or_else(|| Stop::Failed("createDatabase in tenant B gave no node ID".into()))
    }

    /// The file path of B's database, as B's manager sees it.
    async fn sqlite_path_b(c: &mut Ctx, db: &str) -> Step<String> {
        let member_b = c.w.b.principal(M).clone();
        let start = std::time::Instant::now();
        loop {
            let spec = RequestSpec::post(format!("/v1/databases/{db}/query"))
                .tenant(c.w.b.id)
                .json(
                    json!({ "sql": "select file from pragma_database_list where name = 'main'" }),
                );
            let res = client::send(&c.app, &spec, &member_b).await;
            match res.status.as_u16() {
                200 => {
                    let r = Rows::parse(&res)?;
                    let path = r.cell(0, 0)?.as_str().unwrap_or_default().to_string();
                    ensure!(!path.is_empty(), "tenant B's database has no file path");
                    return Ok(path);
                }
                503 if start.elapsed() < READY_TIMEOUT => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                _ => {
                    // The read-only allow-list may leave out database_list: then the engine's
                    // directory is the only source.
                    if let Ok(dir) = std::env::var("RUUM_SQLITE_DIR") {
                        let mut stack = vec![std::path::PathBuf::from(dir)];
                        while let Some(d) = stack.pop() {
                            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                                let p = e.path();
                                if p.is_dir() {
                                    stack.push(p);
                                } else {
                                    return Ok(p.display().to_string());
                                }
                            }
                        }
                    }
                    fail!(
                        "cannot find a file of tenant B ({}); the case needs one",
                        res.status
                    )
                }
            }
        }
    }

    pub fn file_vacuum_into(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let path = std::env::temp_dir().join(format!("ruum-privacy-{}.db", c.tag));
            let p = path.display().to_string();
            refused(c, &f.db, &format!("vacuum into '{p}'")).await?;
            refused(c, &f.db, "vacuum").await?;
            ensure!(!path.exists(), "VACUUM INTO wrote {p}");
            Ok(())
        })
    }

    pub fn file_load_extension(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            refused(
                c,
                &f.db,
                "select load_extension('/usr/lib/sqlite3/pcre.so')",
            )
            .await?;
            refused(c, &f.db, "select load_extension('mod_spatialite')").await?;
            refused(c, &f.db, "select readfile('/etc/passwd')").await?;
            Ok(())
        })
    }

    pub fn file_pragma(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            for sql in [
                "pragma writable_schema = on",
                "pragma trusted_schema = on",
                "pragma journal_mode = delete",
                "pragma cell_size_check = off",
                "select * from pragma_writable_schema",
            ] {
                refused(c, &f.db, sql).await?;
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ pseudonyms

    pub fn pseudonym_direct_call(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let value = c.engine.ph(1);
            let calls = match c.engine {
                Engine::Postgres => vec![
                    format!("select ruum_mask.pseudonym({value}::text)"),
                    format!("select pseudonym({value}::text)"),
                    "select ruum_mask.pseudonym(email) from people".to_string(),
                ],
                Engine::Sqlite => vec![
                    format!("select ruum_pseudonym({value})"),
                    "select ruum_pseudonym(email) from people".to_string(),
                ],
            };
            for sql in calls {
                for who in [A, V, M] {
                    let params = if sql.contains(&value) {
                        vec![json!("guess")]
                    } else {
                        vec![]
                    };
                    let res = c.query(who, &f.db, &sql, params, false).await?;
                    want_any(&res, REFUSED, &format!("{sql} as {}", who.name()))?;
                }
            }
            Ok(())
        })
    }

    pub fn pseudonym_constant(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let sql = format!("select 'guess', {}", c.engine.ph(1));
            let r = c.rows(A, &f.db, &sql, vec![json!("guess-2")]).await?;
            ensure!(
                r.cell(0, 0)? == json!("guess"),
                "a constant comes back as is"
            );
            ensure!(
                r.cell(0, 1)? == json!("guess-2"),
                "a parameter comes back as is"
            );
            want_column(&r, 0, "normal", "none", "a constant")?;
            want_column(&r, 1, "normal", "none", "a parameter")?;
            ensure!(
                pseudonym_id(&r.cell(0, 0)?).is_none(),
                "a constant is never a pseudonym"
            );
            Ok(())
        })
    }

    pub fn pseudonym_key_unreadable(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            for sql in [
                "select * from ruum_mask.key",
                "select key from ruum_mask.key limit 1",
            ] {
                refused(c, &f.db, sql).await?;
            }
            let n = c
                .count(A, &f.db, "select count(*) from pg_seclabels", vec![])
                .await;
            match n {
                Ok(n) => ensure!(n == 0, "pg_seclabels has {n} rows for the agent"),
                Err(Stop::Failed(_)) => {
                    let res = c
                        .query(A, &f.db, "select count(*) from pg_seclabels", vec![], false)
                        .await?;
                    want_any(&res, REFUSED, "pg_seclabels")?;
                }
                Err(other) => return Err(other),
            }
            let res = c
                .query(
                    A,
                    &f.db,
                    "select name, setting from pg_settings where name like 'ruum%' or name like '%mask%' \
                     or name like '%pseudonym%' or name like '%key%'",
                    vec![],
                    false,
                )
                .await?;
            if res.status.as_u16() == 200 {
                for row in Rows::parse(&res)?.rows() {
                    let name = row[0].as_str().unwrap_or_default();
                    ensure!(
                        !name.starts_with("ruum")
                            && !name.contains("mask")
                            && !name.contains("pseudonym"),
                        "a Ruum setting is readable: {name}"
                    );
                }
            }
            let sql = "select current_setting('ruum.key', true), \
                       current_setting('ruum.pseudonym_key', true)";
            let res = c.query(A, &f.db, sql, vec![], false).await?;
            if res.status.as_u16() == 200 {
                let r = Rows::parse(&res)?;
                ensure!(
                    r.cell(0, 0)?.is_null() && r.cell(0, 1)?.is_null(),
                    "a key setting is readable"
                );
            } else {
                want_any(&res, REFUSED, "current_setting of a key")?;
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ leaks

    /// Fails unless the answer is a sanitized `query_failed`: no engine `DETAIL`, no row value in
    /// `message` or `hint`, also for an agent that may see personal values.
    fn sanitized(c: &Ctx, res: &Response, what: &str, value: &str) -> Step<()> {
        want(res, 400, Some("query_failed"), what)?;
        let v = res.json().unwrap_or_default();
        let text = format!("{} {}", v["error"]["message"], v["error"]["hint"]);
        ensure!(
            !text.contains("DETAIL") && !text.contains("Key (") && !text.contains("Failing row"),
            "{what}: the engine's DETAIL is in the error"
        );
        ensure!(!text.contains(value), "{what}: the value is in the error");
        ensure!(
            leaks_in_text(&text, &c.all_markers()).is_empty(),
            "{what}: a row value is in the error"
        );
        Ok(())
    }

    pub fn leak_unique_violation(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let e = c.engine;
            let dup = f.people[0].email.clone();
            let sql = format!(
                "insert into people (id, email, handle, score) values (100, {}, 'h', 1)",
                e.ph(1)
            );
            let res = c.query(E, &f.db, &sql, vec![json!(dup)], true).await?;
            sanitized(
                c,
                &res,
                "a unique violation by a person without manage",
                &dup,
            )?;
            c.grant(&f.db, "write_live").await?;
            c.grant(&f.db, "read_personal").await?;
            let res = c.query(A, &f.db, &sql, vec![json!(dup)], true).await?;
            sanitized(c, &res, "a unique violation by the agent", &dup)?;
            Ok(())
        })
    }

    pub fn leak_check_violation(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            // Postgres writes the whole row into DETAIL ("Failing row contains ..."), with the
            // personal and secret values of the row.
            let sql = "update people set score = -1 where id = 1";
            let res = c.query(E, &f.db, sql, vec![], true).await?;
            sanitized(
                c,
                &res,
                "a check violation by a person without manage",
                &f.people[0].pin,
            )?;
            c.grant(&f.db, "write_live").await?;
            let res = c.query(A, &f.db, sql, vec![], true).await?;
            sanitized(c, &res, "a check violation by the agent", &f.people[0].pin)?;
            Ok(())
        })
    }

    /// A migration of `member` that fails with a unique violation that names a raw email.
    async fn failed_migration(c: &mut Ctx, f: &Fixture) -> Step<()> {
        let res = c
            .migrate(
                M,
                &f.db,
                "insert into people (id, email, handle, pin, score) \
                 select 200, email, 'h', 'p', 1 from people where id = 1;",
                json!({}),
            )
            .await?;
        // A failed migration answers an error, or 200 with the status `failed`.
        let status = res
            .json()
            .and_then(|v| v["migration"]["status"].as_str().map(str::to_string));
        ensure!(
            !res.status.is_success() || status.as_deref() == Some("failed"),
            "the failing migration must fail, got {} {status:?}",
            res.status
        );
        // The database may be `ready` again or `degraded`; either way the rows must be clean.
        let _ = c.wait_ready(&f.db).await;
        Ok(())
    }

    pub fn leak_failed_migration(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            failed_migration(c, &f).await?;
            for who in [V, E, A] {
                let spec = RequestSpec::get(c.path(&f.db, "/migrations"));
                // Sanitized for people without manage and agents: no marker of any class.
                let all = c.all_markers();
                let res = c.send_checked(who, spec, &all).await?;
                want(
                    &res,
                    200,
                    None,
                    &format!("listDatabaseMigrations as {}", who.name()),
                )?;
                let v = res.json().unwrap_or_default();
                let failed = v["items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|m| m["status"] == "failed");
                ensure!(failed, "the failed migration is listed for {}", who.name());
                ensure!(
                    !res.text().contains("DETAIL") && !res.text().contains("Key ("),
                    "the engine's DETAIL is in the migration error"
                );
            }
            Ok(())
        })
    }

    pub fn leak_status_message(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            failed_migration(c, &f).await?;
            // The status message holds no row value, also for the manager.
            for who in [M, V, A] {
                let spec = RequestSpec::get(c.path(&f.db, ""));
                let all = c.all_markers();
                let res = c.send_checked(who, spec, &all).await?;
                want(&res, 200, None, &format!("getDatabase as {}", who.name()))?;
                let s = res.json().unwrap_or_default()["status"].clone();
                let text = s.to_string();
                ensure!(
                    !text.contains("DETAIL") && !text.contains("Key ("),
                    "the status holds the engine's DETAIL: {}",
                    s["reason"]
                );
            }
            Ok(())
        })
    }

    pub fn leak_log_literals(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let literal = f.people[2].email.clone();
            let sql = format!(
                "select count(*) from people where email = '{literal}' or handle = 'known-literal'"
            );
            let n = c.count(M, &f.db, &sql, vec![]).await?;
            ensure!(n == 1, "control: the person's literal finds the row");
            // The manager's own log: the statement is normalized.
            let entries = c.log(M, &f.db).await?;
            let mine: Vec<&Value> = entries
                .iter()
                .filter(|e| e["statement"].as_str().is_some_and(|s| s.contains("$L")))
                .collect();
            ensure!(
                !mine.is_empty(),
                "the log has the normalized statement ($L1, ...)"
            );
            let text = entries
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(
                !text.contains(&literal) && !text.contains("known-literal"),
                "the log holds a literal"
            );
            // The agent with manage sees only its own entries.
            let _ = c
                .rows(A, &f.db, "select count(*) from orders", vec![])
                .await?;
            let agent = c.w.a.principal(A).id.to_string();
            let theirs = c.log(A, &f.db).await?;
            ensure!(!theirs.is_empty(), "the agent sees its own entry");
            for e in &theirs {
                ensure!(
                    e["by"]["actor"]["id"] == agent.as_str(),
                    "an agent with manage sees only its own entries"
                );
            }
            // A person with view sees their own entries and their agents' only.
            let viewer = c.log(V, &f.db).await?;
            ensure!(
                viewer.is_empty(),
                "member_view ran nothing and has no agent, but sees {} entries",
                viewer.len()
            );
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ side channels

    pub fn side_pg_stats(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            // Make statistics as the manager; whether the platform allows ANALYZE does not matter.
            let _ = c.query(M, &f.db, "analyze people", vec![], true).await?;
            let res = c
                .query(
                    A,
                    &f.db,
                    "select attname::text, most_common_vals::text, histogram_bounds::text \
                     from pg_stats where tablename in ('people', 'orders')",
                    vec![],
                    false,
                )
                .await?;
            if res.status.as_u16() == 200 {
                let text = res.text();
                ensure!(
                    !text.contains("known-pin-") && !text.contains("known-handle-"),
                    "pg_stats shows values to the agent"
                );
            } else {
                want_any(&res, REFUSED, "pg_stats")?;
            }
            Ok(())
        })
    }

    pub fn side_explain(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            for sql in [
                "explain select * from people where email like 'a%'",
                "explain (verbose, costs off) select email, pin from people order by email",
                "explain (analyze, verbose) select * from people p join orders o on o.pin_copy = p.pin",
            ] {
                let res = c.query(A, &f.db, sql, vec![], false).await?;
                if res.status.as_u16() == 200 {
                    let text = res.text();
                    ensure!(
                        !text.contains("known-pin-") && !text.contains("known-handle-"),
                        "{sql} shows values"
                    );
                } else {
                    want_any(&res, REFUSED, sql)?;
                }
            }
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ pseudonym oracles

    pub fn poracle_log_param(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let shared = f.shared().clone();
            let lens = c
                .lens_pseudonym(&f.db, "people", "email", shared.id)
                .await?;
            let sql = format!(
                "select count(*) from people where email = {}",
                c.engine.ph(1)
            );
            let _ = c.count(A, &f.db, &sql, vec![json!(shared.email)]).await?;
            let entries = c.log(A, &f.db).await?;
            let mut seen = false;
            for e in &entries {
                for p in e["params"].as_array().into_iter().flatten() {
                    seen = true;
                    ensure!(
                        is_hidden(p, "personal"),
                        "a logged parameter is a hidden marker, got {p}"
                    );
                    ensure!(
                        pseudonym_id(p).is_none(),
                        "a logged parameter is never a pseudonym"
                    );
                    ensure!(
                        !p.to_string().contains(&lens),
                        "a logged parameter equals the lens pseudonym"
                    );
                }
            }
            ensure!(seen, "the log has the agent's parameter");
            let text = entries
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            ensure!(
                !text.contains(&shared.email) && !text.contains(&lens),
                "the log holds the value or its pseudonym"
            );
            Ok(())
        })
    }

    pub fn poracle_approval_param(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let shared = f.shared().clone();
            let lens = c
                .lens_pseudonym(&f.db, "people", "email", shared.id)
                .await?;
            let (_, p) = approval_of(c, &f, &shared.email).await?;
            ensure!(
                !p.to_string().contains(&lens),
                "the approval parameter equals the lens pseudonym"
            );
            Ok(())
        })
    }

    /// The insert of the shared value as row `id`, for a migration on a branch.
    fn insert_shared(id: i64, email: &str) -> String {
        format!(
            "insert into people (id, email, handle, pin, score) values ({id}, '{email}', 'h-{id}', 'p-{id}', 1)"
        )
    }

    /// The three ways an agent puts the shared value into a draft branch, and the comparison of
    /// the branch's pseudonym with the source's.
    async fn draft_branch_key(c: &mut Ctx, data: &str) -> Step<Db> {
        let f = c.fixture().await?;
        let shared = f.shared().clone();
        let src = c
            .lens_pseudonym(&f.db, "people", "email", shared.id)
            .await?;
        let b = c.branch(&f.db, data).await?;
        // (1) Rows the agent writes with a statement: refused (a write of a personal column
        // needs read_personal), or written and then with the branch's pseudonym.
        let sql = format!(
            "insert into people (id, email, handle, score) values (301, {}, 'h', 1)",
            c.engine.ph(1)
        );
        let res = c
            .query(A, &b, &sql, vec![json!(shared.email)], true)
            .await?;
        if res.status.is_success() {
            let p = c.lens_pseudonym(&b, "people", "email", 301).await?;
            ensure!(
                p != src,
                "a row the agent wrote into the {data} branch gives the source's pseudonym"
            );
        } else {
            want(
                &res,
                403,
                Some("class_read_required"),
                "the agent's insert into the branch",
            )?;
        }
        // (2) A migration that inserts rows (it runs as owner, without approval on a draft).
        let res = c
            .migrate(A, &b, &insert_shared(302, &shared.email), json!({}))
            .await?;
        want(
            &res,
            200,
            None,
            &format!("the agent's migration on the {data} branch"),
        )?;
        c.wait_ready(&b).await?;
        let p = c.lens_pseudonym(&b, "people", "email", 302).await?;
        ensure!(
            p != src,
            "a migration on the {data} branch gives the source's pseudonym"
        );
        // (3) A DO block: in a migration it runs; in a query it is refused or gives the
        // branch's pseudonym.
        if c.engine == Engine::Postgres {
            let up = format!("do $$ begin {}; end $$;", insert_shared(303, &shared.email));
            let res = c.migrate(A, &b, &up, json!({})).await?;
            want(
                &res,
                200,
                None,
                "a DO block in the agent's migration on the branch",
            )?;
            c.wait_ready(&b).await?;
            let p = c.lens_pseudonym(&b, "people", "email", 303).await?;
            ensure!(
                p != src,
                "a DO block on the {data} branch gives the source's pseudonym"
            );
            let res = c
                .query(
                    A,
                    &b,
                    &format!("do $$ begin {}; end $$", insert_shared(304, &shared.email)),
                    vec![],
                    true,
                )
                .await?;
            if res.status.is_success() {
                let p = c.lens_pseudonym(&b, "people", "email", 304).await?;
                ensure!(
                    p != src,
                    "a DO block query on the {data} branch gives the source's pseudonym"
                );
            } else {
                want_any(&res, REFUSED, "a DO block query on the branch")?;
            }
        }
        Ok(b)
    }

    pub fn poracle_masked_branch(c: &mut Ctx) -> F<'_> {
        Box::pin(async move { draft_branch_key(c, "masked").await.map(|_| ()) })
    }

    pub fn poracle_schema_only_branch(c: &mut Ctx) -> F<'_> {
        Box::pin(async move { draft_branch_key(c, "schema_only").await.map(|_| ()) })
    }

    pub fn poracle_copy_keeps_key(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let b = draft_branch_key(c, "masked").await?;
            let in_branch = c.lens_pseudonym(&b, "people", "email", 302).await?;
            // A person's copy (full data from a draft branch is still the branch's data).
            let copy = c.copy(M, &b, Data::Masked).await?;
            let in_copy = c.lens_pseudonym(&copy, "people", "email", 302).await?;
            ensure!(
                in_copy == in_branch,
                "a copy of a masked branch must keep the branch's key"
            );
            Ok(())
        })
    }

    // ------------------------------------------------------------------------ exports

    /// The export of `db` as `who`. No public operation starts one yet (see [`WAIT_EXPORT`]);
    /// the case is planned until one exists, and this helper says so if it is reached.
    async fn export(_c: &mut Ctx, _who: Who, _db: &Db, data: &str) -> Step<String> {
        Err(Stop::Planned(format!(
            "no public export operation for a {data} export (contract gap for the panel)"
        )))
    }

    /// The checks of decision 0004-29 on an export.
    fn export_clean(c: &Ctx, text: &str, masked: bool) -> Step<()> {
        for name in [
            "ruum_mask",
            "ruum_masked",
            "ruum_nosecret",
            "ruum_pseudonym",
        ] {
            ensure!(!text.contains(name), "the export holds {name}");
        }
        ensure!(
            !text.contains("CREATE ROLE") && !text.contains("GRANT "),
            "the export holds roles or grants"
        );
        if masked {
            ensure!(
                leaks_in_text(text, &c.all_markers()).is_empty(),
                "a masked export holds a raw value"
            );
        }
        Ok(())
    }

    pub fn export_person_full(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let text = export(c, M, &f.db, "full").await?;
            export_clean(c, &text, false)
        })
    }

    pub fn export_person_masked(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let text = export(c, M, &f.db, "masked").await?;
            export_clean(c, &text, true)
        })
    }

    pub fn export_agent(c: &mut Ctx) -> F<'_> {
        Box::pin(async move {
            let f = c.fixture().await?;
            let text = export(c, A, &f.db, "masked").await?;
            export_clean(c, &text, true)
        })
    }
}

// ------------------------------------------------------------------------------- the runner

/// The report of a run, with the reasons of the planned cases.
pub struct PrivacyReport {
    pub report: Report,
    /// Reason -> the case runs (`<case>@<engine>`) planned for it.
    pub planned: BTreeMap<String, Vec<String>>,
}

impl PrivacyReport {
    /// The number of planned case runs.
    pub fn planned_count(&self) -> usize {
        self.planned.values().map(Vec::len).sum()
    }

    pub fn render(&self) -> String {
        let mut s = self.report.render();
        if !self.planned.is_empty() {
            s.push_str(&format!(
                "\n{} case runs planned, by reason:\n",
                self.planned_count()
            ));
            for (why, runs) in &self.planned {
                s.push_str(&format!(
                    "  PLANNED  {why}:\n           {}\n",
                    runs.join(", ")
                ));
            }
        }
        s
    }

    /// Prints the report and panics if the suite fails.
    pub fn finish(self) {
        let text = self.render();
        println!("{text}");
        assert!(self.report.ok(), "{} failed:{text}", self.report.suite);
    }
}

/// The cases selected by [`CASES_ENV`].
pub fn selected_cases() -> Vec<Case> {
    let filter: Vec<String> = std::env::var(CASES_ENV)
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    cases()
        .into_iter()
        .filter(|c| filter.is_empty() || filter.iter().any(|p| c.id.starts_with(p.as_str())))
        .collect()
}

/// Runs the suite on the backend of `RUUM_SUITE_BACKEND`.
pub async fn run() -> PrivacyReport {
    let backend = Backend::start().await;
    let report = run_on(&backend).await;
    backend.finish().await;
    report
}

/// Runs one case on one engine, in a world of its own.
async fn run_one(backend: &Backend, case: Case, engine: Engine) -> (Outcome, Option<String>) {
    if let Some(why) = case.planned_reason(engine, backend.kind()) {
        return (Outcome::Planned, Some(why));
    }
    // Without the data cluster the Postgres backend gives `postgres` manifests the memory
    // engine, on which no privacy case may run (MEMORY_ENGINE). Planned with the reason, or a
    // failure where the data cluster is required (CI, RUUM_DBENGINE_REQUIRE_POSTGRES).
    let kind = match engine {
        Engine::Postgres => ruum_core::databases::engine::EngineKind::Postgres,
        Engine::Sqlite => ruum_core::databases::engine::EngineKind::Sqlite,
    };
    if !backend.engine_has_lens(kind) {
        let why = format!(
            "no {} engine with a lens in this run: postgres manifests use the memory engine \
             (set RUUM_DBHOST_URL to the data cluster)",
            engine.name()
        );
        let required = std::env::var_os("CI").is_some()
            || std::env::var_os("RUUM_DBENGINE_REQUIRE_POSTGRES").is_some();
        return if required {
            (Outcome::Failed(why), None)
        } else {
            (Outcome::Planned, Some(why))
        };
    }
    let (state, db) = backend.world().await;
    let w = fixtures::seed(&state).await;
    let result = if !w.seeded {
        Err(Stop::Failed("the fixture world is not seeded".into()))
    } else {
        let mut ctx = Ctx::new(case.id, engine, backend.kind(), client::app(state), w);
        match (case.run)(&mut ctx).await {
            Ok(()) => ctx.sweep().await,
            Err(e) => {
                // A failed case still must not have stored a raw value.
                match ctx.sweep().await {
                    Err(Stop::Failed(s)) => Err(match e {
                        Stop::Failed(f) => Stop::Failed(format!("{f}; {s}")),
                        _ => Stop::Failed(s),
                    }),
                    _ => Err(e),
                }
            }
        }
    };
    if let Some(db) = db {
        db.finish().await;
    }
    match result {
        Ok(()) => (Outcome::Passed, None),
        Err(Stop::Failed(s)) => (Outcome::Failed(scrub(&s)), None),
        Err(Stop::Pending(s)) => (Outcome::Pending, Some(scrub(&s))),
        Err(Stop::Planned(s)) => (Outcome::Planned, Some(s)),
    }
}

/// Runs the suite on `backend`.
pub async fn run_on(backend: &Backend) -> PrivacyReport {
    let mut report = Report::new(&format!("privacy suite [{}]", backend.name()));
    let runs: Vec<(Case, Engine)> = selected_cases()
        .into_iter()
        .flat_map(|c| c.engines.iter().map(move |e| (c, *e)))
        .collect();
    let results: Vec<(String, String, Outcome, Option<String>)> = stream::iter(runs)
        .map(|(case, engine)| async move {
            let (o, why) = run_one(backend, case, engine).await;
            (
                format!("{}@{}", case.id, engine.name()),
                case.title.to_string(),
                o,
                why,
            )
        })
        .buffered(backend.jobs())
        .collect()
        .await;
    let mut planned: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (run, title, outcome, why) in results {
        let label = match (&outcome, &why) {
            (Outcome::Pending, Some(w)) => format!("{title} (pending: {w})"),
            _ => title,
        };
        if outcome == Outcome::Planned {
            planned
                .entry(why.unwrap_or_else(|| "no reason given".into()))
                .or_default()
                .push(run.clone());
        }
        report.add(&run, label, outcome);
    }
    PrivacyReport { report, planned }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    fn res(body: &str, headers: &[(&'static str, &str)]) -> Response {
        let mut h = HeaderMap::new();
        for (k, v) in headers {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        Response {
            status: StatusCode::BAD_REQUEST,
            headers: h,
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn the_leak_helper_looks_everywhere() {
        let m = vec!["ruum-raw-t1-p".to_string()];
        // In a cell, in the error hint, in marker JSON, escaped, and in a header.
        assert!(!leaks(&res(r#"{"rows":[["a.ruum-raw-t1-p1@x"]]}"#, &[]), &m).is_empty());
        assert!(
            !leaks(
                &res(
                    r#"{"error":{"code":"query_failed","message":"x","hint":"ruum-raw-t1-p2"}}"#,
                    &[]
                ),
                &m
            )
            .is_empty()
        );
        assert!(
            !leaks(
                &res(r#"{"$ruum":"literal","value":{"k":"ruum-raw-t1-p3"}}"#, &[]),
                &m
            )
            .is_empty()
        );
        assert!(!leaks(&res(r#"{"x":"ruum-raw-t1-p4"}"#, &[]), &m).is_empty());
        assert!(!leaks(&res("{}", &[("x-debug", "ruum-raw-t1-p5")]), &m).is_empty());
        assert!(
            leaks(
                &res(
                    r#"{"rows":[[{"$ruum":"pseudonym","id":"p_0123456789ab"}]]}"#,
                    &[]
                ),
                &m
            )
            .is_empty()
        );
        // Another run's marker is not this run's.
        assert!(leaks(&res(r#"{"x":"ruum-raw-t2-p1"}"#, &[]), &m).is_empty());
    }

    #[test]
    fn excerpts_hold_no_marker() {
        let r = res(
            r#"{"error":{"message":"Key (email)=(a.ruum-raw-t1-p1@example.test)"}}"#,
            &[],
        );
        let e = excerpt(&r);
        assert!(!e.contains("ruum-raw-"), "{e}");
        assert!(e.contains("<marker>"));
    }

    #[test]
    fn pseudonym_shape_comes_from_the_core_function() {
        let ex = shape_example();
        assert!(ex.starts_with("p_") && ex.len() == 2 + ruum_core::privacy::PSEUDONYM_HEX_LEN);
        assert!(pseudonym_id(&json!({"$ruum": "pseudonym", "id": ex})).is_some());
        // Other shapes are not the platform's pseudonyms.
        assert!(pseudonym_id(&json!({"$ruum": "pseudonym", "id": "p_0123"})).is_none());
        assert!(
            pseudonym_id(&json!({"$ruum": "pseudonym", "id": "p_0123456789ab", "x": 1})).is_none()
        );
        assert!(pseudonym_id(&json!("p_0123456789ab")).is_none());
        assert!(pseudonym_id(&json!({"$ruum": "hidden", "class": "personal"})).is_none());
        assert!(is_hidden(
            &json!({"$ruum": "hidden", "class": "secret"}),
            "secret"
        ));
        assert!(!is_hidden(
            &json!({"$ruum": "hidden", "class": "secret", "v": 1}),
            "secret"
        ));
        assert!(is_masked(&json!("p_0123456789ab")));
        assert!(!is_masked(&json!("guess")));
    }

    #[test]
    fn a_role_name_in_an_answer_is_found() {
        let rows = |v: Value| Rows {
            value: json!({ "columns": [], "rows": v, "content_trust": "untrusted" }),
        };
        let before = json!("r_00aa11bb");
        assert_eq!(
            cases::other_role_in(&rows(json!([["r_00aa11bb"]])), &before),
            None
        );
        assert_eq!(
            cases::other_role_in(&rows(json!([["x", "ruum_dbhost"]])), &before),
            Some("ruum_dbhost".to_string())
        );
        assert_eq!(
            cases::other_role_in(&rows(json!([[1], ["r_ff09"]])), &before),
            Some("r_ff09".to_string())
        );
        assert_eq!(
            cases::other_role_in(&rows(json!([["r_owner", "nick-1"]])), &before),
            None
        );
    }

    #[test]
    fn the_filter_cases_wait_for_their_engine_card() {
        let by_id: BTreeMap<&str, Case> = cases().into_iter().map(|c| (c.id, c)).collect();
        // A lens case waits for Q-2 on Postgres and Q-3 on SQLite, on both backends, also when
        // its operations exist; a case without the filter does not.
        // Q-2 and Q-3 remove their entries; the check holds while an entry is there.
        let lens = by_id["query.plain_column"];
        for engine in [Engine::Postgres, Engine::Sqlite] {
            if let Some(card) = filter_waits_for(engine) {
                let why = lens
                    .planned_reason(engine, Kind::Postgres)
                    .unwrap_or_default();
                assert!(why.contains(&format!("waits for {card}")), "{why}");
            }
        }
        let schema = by_id["schema.names_only"];
        let pg = schema
            .planned_reason(Engine::Postgres, Kind::Postgres)
            .unwrap_or_default();
        assert!(!pg.contains("privacy filter"), "{pg}");
        // Every memory-lens case needs the filter.
        for c in by_id.values() {
            if c.memory == MEMORY_LENS {
                assert_eq!(c.filter, Filter::Needed, "{}", c.id);
            }
        }
    }

    #[test]
    fn visibility_follows_the_rules() {
        let w = World {
            a: crate::fixtures::TenantFixture::placeholder_for_tests("alpha"),
            b: crate::fixtures::TenantFixture::placeholder_for_tests("bravo"),
            seeded: false,
            bridge: None,
        };
        let app = client::app(ruum_api::AppState::in_memory());
        let mut c = Ctx::new("t", Engine::Postgres, Kind::Memory, app, w);
        let live = Db {
            id: "x".into(),
            data: Data::Real,
        };
        let masked = Db {
            id: "y".into(),
            data: Data::Masked,
        };
        let (p, s) = (c.personal_prefix(), c.secret_prefix());
        assert_eq!(c.forbidden(Who::Member, &live), Vec::<String>::new());
        assert_eq!(c.forbidden(Who::MemberView, &live), vec![s.clone()]);
        assert_eq!(c.forbidden(Who::MemberEdit, &live), vec![s.clone()]);
        assert_eq!(
            c.forbidden(Who::AgentOut, &live),
            vec![p.clone(), s.clone()]
        );
        assert_eq!(
            c.forbidden(Who::Member, &masked),
            vec![p.clone(), s.clone()]
        );
        c.read_personal.insert("x".into());
        assert_eq!(c.forbidden(Who::AgentOut, &live), vec![s.clone()]);
        assert_eq!(c.forbidden(Who::AgentOut, &masked), vec![p, s]);
    }
}
