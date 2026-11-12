//! The acceptance cases of the 24 database operations (card T-1, `docs/STEP2.md`), written from
//! the contract (`contracts/openapi.yaml`, tag `databases`) and decision 0004 before the code.
//!
//! Each case is a short scenario through the API, in a world of its own
//! ([`Backend::world`]), on the backend of the suites (`RUUM_SUITE_BACKEND=memory|postgres`).
//! A case names every operation it calls ([`Case::ops`]):
//!
//! - **planned**: while any of its operations has `x-ruum-status: planned` in the contract, the
//!   case does not run and is reported as planned (decision 0004-21), also with
//!   `RUUM_REQUIRE_IMPLEMENTED=1`. When the card that builds the operations removes their marks,
//!   the case runs without a change here.
//! - **pending (blocked)**: a case in [`PENDING`] waits for a named card (for example the
//!   branches of card D-3). It does not run and never fails the suite, also
//!   not with `RUUM_REQUIRE_IMPLEMENTED=1`. The card that unblocks it removes its entry.
//! - **pending (501)**: a handler answered `501`. It fails only with
//!   `RUUM_REQUIRE_IMPLEMENTED=1`, like in the isolation and permission suites.
//! - **passed** or **failed** otherwise. A case that calls an operation it does not name fails,
//!   so the planned gate cannot be skipped by mistake.
//!
//! Every answer of every case is also checked for one rule of decision 0004-35: an answer with
//! `database_not_ready` is `503` with a `Retry-After` header.
//!
//! The groups are in the submodules; each test binary `tests/databases_<group>.rs` runs one
//! group, and `tests/databases_meta.rs` checks the cases themselves.

#![allow(dead_code)]

pub mod agents;
pub mod api;
pub mod apps;
pub mod apps_archive;
pub mod apps_deploy;
pub mod apps_publish;
pub mod apps_roles;
pub mod apps_settings;
pub mod d1;
pub mod guests;
pub mod nodes;
pub mod statements;

use axum::Router;
use axum::http::StatusCode;
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use ruum_testkit::client::{self, RequestSpec, Response};
use ruum_testkit::contract::{self, Operation};
use ruum_testkit::fixtures::{self, Backend, Principal, TenantFixture, Who, World};
use ruum_testkit::report::{REQUIRE_IMPLEMENTED_ENV, require_implemented};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};

/// The cases that wait for a card other than the one that removes the `planned` marks of their
/// operations, with that card. They do not run, and they never fail the suite (also not with
/// `RUUM_REQUIRE_IMPLEMENTED=1`). The card named last unblocks the case and removes its line.
///
/// Card P-3 added the typed-node rules to the node stores (decisions 0004-6, 0004-7 and
/// 0004-18) and removed its entries. The cases below need database branches, which card D-3
/// makes (`createDatabaseBranch`); their typed-node rules (a branch stays with its database, a
/// copy takes no branch, trash and purge take the branches along) are P-3's and are built.
pub const PENDING: &[(&str, &str)] = &[
    ("a_branch_cannot_move_away", "needs branches: D-3"),
    ("copy_of_a_database_copies_no_branch", "needs branches: D-3"),
    (
        "trash_and_purge_take_the_branches_along",
        "needs branches: D-3",
    ),
];

/// Why a case stopped.
#[derive(Debug)]
pub enum Fail {
    /// The case found a wrong answer.
    Failed(String),
    /// A handler answered `501`: the operation is routed but not built yet.
    Pending(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Failed(s)
    }
}

pub type CaseResult = Result<(), Fail>;

/// Fails the case with a formatted message.
#[macro_export]
macro_rules! fail {
    ($($t:tt)*) => {
        return Err($crate::common::Fail::Failed(format!($($t)*)))
    };
}

/// Fails the case when the condition is false.
#[macro_export]
macro_rules! check {
    ($cond:expr, $($t:tt)*) => {
        if !$cond {
            return Err($crate::common::Fail::Failed(format!($($t)*)));
        }
    };
}

/// Declares a case: `case!("name", ["op", ...], "what it checks", |cx| { ... })`.
#[macro_export]
macro_rules! case {
    ($name:expr, [$($op:expr),+ $(,)?], $note:expr, |$cx:ident| $body:block) => {
        $crate::common::Case {
            name: $name,
            ops: &[$($op),+],
            note: $note,
            run: |$cx: $crate::common::Ctx| -> futures::future::BoxFuture<'static, $crate::common::CaseResult> {
                Box::pin(async move $body)
            },
        }
    };
}

/// One acceptance case.
pub struct Case {
    /// A unique snake_case name.
    pub name: &'static str,
    /// Every operation (`operationId`) the case calls, set-up steps included.
    pub ops: &'static [&'static str],
    /// What the case checks, and the contract text or decision it comes from.
    pub note: &'static str,
    pub run: fn(Ctx) -> BoxFuture<'static, CaseResult>,
}

/// What a case reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    /// The named operations are planned in the contract.
    Planned(Vec<String>),
    /// The case waits for a card ([`PENDING`]).
    Blocked(&'static str),
    /// A handler answered `501`.
    Pending(String),
    Failed(String),
}

/// The decision before a case runs: planned while any operation is planned, then blocked when
/// [`PENDING`] lists it, else `None` (the case runs).
pub fn gate(case: &str, ops: &[&str], is_planned: impl Fn(&str) -> bool) -> Option<Outcome> {
    let planned: Vec<String> = ops
        .iter()
        .filter(|op| is_planned(op))
        .map(|op| op.to_string())
        .collect();
    if !planned.is_empty() {
        return Some(Outcome::Planned(planned));
    }
    pending_reason(case).map(Outcome::Blocked)
}

/// The reason of a case in [`PENDING`].
pub fn pending_reason(case: &str) -> Option<&'static str> {
    PENDING.iter().find(|(c, _)| *c == case).map(|(_, r)| *r)
}

/// Every case of every group.
pub fn all_cases() -> Vec<Case> {
    let mut v = d1::cases();
    v.extend(guests::cases());
    v.extend(agents::cases());
    v.extend(nodes::cases());
    v.extend(statements::cases());
    v
}

/// The world of one case: the router, the fixture world and the operations the case called.
#[derive(Clone)]
pub struct Ctx {
    pub app: Router,
    pub w: World,
    pub state: ruum_api::AppState,
    fallback: Arc<Response>,
    called: Arc<Mutex<BTreeSet<String>>>,
}

/// The operation of `contracts/openapi.yaml` that a request calls. Where two templates match
/// (`/v1/nodes/root` and `/v1/nodes/{node_id}`), the one with fewer parameters wins.
pub fn operation_of(method: &str, uri: &str) -> Option<String> {
    static OPS: OnceLock<Vec<Operation>> = OnceLock::new();
    OPS.get_or_init(contract::operations)
        .iter()
        .filter(|o| o.method == method && o.matches(uri))
        .min_by_key(|o| o.path.matches('{').count())
        .map(|o| o.id.clone())
}

impl Ctx {
    /// Tenant A of the fixture: every case works in it.
    pub fn a(&self) -> &TenantFixture {
        &self.w.a
    }

    pub fn p(&self, who: Who) -> &Principal {
        self.w.a.principal(who)
    }

    /// Sends `req` as `who` (any principal). Fails the case on `500`, on a missing route, on a
    /// `database_not_ready` answer that is not `503` with `Retry-After`, and reports `501` as
    /// pending.
    pub async fn call(&self, who: &Principal, req: RequestSpec) -> Result<Response, Fail> {
        let label = format!("{} as {}", req.label(), who.who.name());
        if let Some(op) = operation_of(req.method.as_str(), &req.uri) {
            self.called.lock().expect("lock").insert(op);
        }
        let res = client::send(&self.app, &req, who).await;
        if client::is_unrouted(&res, &self.fallback) {
            fail!("{label}: no handler answers this method and path");
        }
        if res.is_not_implemented() {
            return Err(Fail::Pending(format!("{label} answers 501")));
        }
        if res.status.is_server_error() && res.status != StatusCode::SERVICE_UNAVAILABLE {
            fail!("{label}: server error {}: {}", res.status, res.text());
        }
        if res.error_code().as_deref() == Some("database_not_ready")
            && (res.status != StatusCode::SERVICE_UNAVAILABLE
                || !res.headers.contains_key("retry-after"))
        {
            fail!(
                "{label}: database_not_ready must be 503 with Retry-After (decision 0004-35), got \
                 {} with headers {:?}",
                res.status,
                res.headers.keys().collect::<Vec<_>>()
            );
        }
        if res.status == StatusCode::SERVICE_UNAVAILABLE
            && res.error_code().as_deref() != Some("database_not_ready")
        {
            fail!("{label}: unexpected 503: {}", res.text());
        }
        Ok(res)
    }

    /// [`Ctx::call`] as a principal of tenant A.
    pub async fn send(&self, who: Who, req: RequestSpec) -> Result<Response, Fail> {
        let p = self.p(who).clone();
        self.call(&p, req).await
    }

    /// Sends and expects one of `statuses`; returns the JSON body (`null` when there is none).
    pub async fn ok(&self, who: Who, req: RequestSpec, statuses: &[u16]) -> Result<Value, Fail> {
        let p = self.p(who).clone();
        self.ok_as(&p, req, statuses).await
    }

    pub async fn ok_as(
        &self,
        who: &Principal,
        req: RequestSpec,
        statuses: &[u16],
    ) -> Result<Value, Fail> {
        let label = format!("{} as {}", req.label(), who.who.name());
        let res = self.call(who, req).await?;
        check!(
            statuses.contains(&res.status.as_u16()),
            "{label}: got {}, expected {statuses:?}: {}",
            res.status,
            res.text()
        );
        Ok(res.json().unwrap_or(Value::Null))
    }

    /// Sends and expects `status` with an error code in `codes` (any code when empty); returns
    /// the `error` object.
    pub async fn refused(
        &self,
        who: Who,
        req: RequestSpec,
        status: u16,
        codes: &[&str],
    ) -> Result<Value, Fail> {
        let p = self.p(who).clone();
        self.refused_as(&p, req, status, codes).await
    }

    pub async fn refused_as(
        &self,
        who: &Principal,
        req: RequestSpec,
        status: u16,
        codes: &[&str],
    ) -> Result<Value, Fail> {
        let label = format!("{} as {}", req.label(), who.who.name());
        let res = self.call(who, req).await?;
        let code = res.error_code().unwrap_or_default();
        check!(
            res.status.as_u16() == status && (codes.is_empty() || codes.contains(&code.as_str())),
            "{label}: got {} {code:?}, expected {status} {codes:?}: {}",
            res.status,
            res.text()
        );
        Ok(res
            .json()
            .map(|v| v["error"].clone())
            .unwrap_or(Value::Null))
    }
}

/// Runs one case in a world of its own on `backend`.
pub async fn run_case(backend: &Backend, case: &Case) -> Outcome {
    if let Some(o) = gate(case.name, case.ops, contract::is_planned) {
        return o;
    }
    let (state, db) = backend.world().await;
    let w = fixtures::seed(&state).await;
    let outcome = if !w.seeded {
        Outcome::Failed("the fixture world did not seed".into())
    } else {
        let app = client::app(state.clone());
        let fallback = client::fallback_fingerprint(&app, w.a.principal(Who::Owner)).await;
        let called = Arc::new(Mutex::new(BTreeSet::new()));
        let cx = Ctx {
            app,
            w,
            state,
            fallback: Arc::new(fallback),
            called: called.clone(),
        };
        // On its own task, so a panic fails this case only.
        let result = tokio::spawn((case.run)(cx)).await;
        let unnamed: Vec<String> = called
            .lock()
            .expect("lock")
            .iter()
            .filter(|op| !case.ops.contains(&op.as_str()))
            .cloned()
            .collect();
        match result {
            Err(e) => Outcome::Failed(format!("the case panicked: {e}")),
            Ok(_) if !unnamed.is_empty() => Outcome::Failed(format!(
                "the case calls operations it does not name in its ops list: {unnamed:?}"
            )),
            Ok(Ok(())) => Outcome::Passed,
            Ok(Err(Fail::Pending(m))) => Outcome::Pending(m),
            Ok(Err(Fail::Failed(m))) => Outcome::Failed(m),
        }
    };
    if let Some(db) = db {
        db.finish().await;
    }
    outcome
}

/// Runs a group of cases on the backend of `RUUM_SUITE_BACKEND`, prints the report and panics
/// when the group fails.
pub async fn run_group(title: &str, cases: Vec<Case>) {
    let backend = Backend::start().await;
    let rows: Vec<(&'static str, Outcome)> = stream::iter(cases.iter())
        .map(|c| {
            let backend = &backend;
            async move { (c.name, run_case(backend, c).await) }
        })
        .buffered(backend.jobs())
        .collect()
        .await;
    let name = backend.name();
    backend.finish().await;
    let (text, ok) = render(&format!("{title} [{name}]"), &rows);
    println!("{text}");
    assert!(ok, "{title} failed:{text}");
}

/// The report of a group, and whether it passes in the current mode.
pub fn render(title: &str, rows: &[(&'static str, Outcome)]) -> (String, bool) {
    let strict = require_implemented();
    let count = |f: fn(&Outcome) -> bool| rows.iter().filter(|r| f(&r.1)).count();
    let passed = count(|o| *o == Outcome::Passed);
    let planned = count(|o| matches!(o, Outcome::Planned(_)));
    let blocked = count(|o| matches!(o, Outcome::Blocked(_)));
    let pending = count(|o| matches!(o, Outcome::Pending(_)));
    let failed = count(|o| matches!(o, Outcome::Failed(_)));
    let mut s = String::new();
    let _ = writeln!(
        s,
        "\n=== {title}: {} cases: {passed} passed, {planned} planned, {blocked} pending (blocked \
         by a card), {pending} pending (501), {failed} failed; {REQUIRE_IMPLEMENTED_ENV}={} ===",
        rows.len(),
        if strict { "1" } else { "0" }
    );
    for (name, o) in rows {
        let line = match o {
            Outcome::Passed => format!("  PASS     {name}"),
            Outcome::Planned(ops) => format!("  PLANNED  {name} (planned: {})", ops.join(", ")),
            Outcome::Blocked(why) => format!("  PENDING  {name} ({why})"),
            Outcome::Pending(why) => format!(
                "  {}  {name} ({why})",
                if strict { "FAIL    " } else { "PENDING " }
            ),
            Outcome::Failed(why) => format!("  FAIL     {name}: {why}"),
        };
        let _ = writeln!(s, "{line}");
    }
    let ok = failed == 0 && !(strict && pending > 0);
    (s, ok)
}
