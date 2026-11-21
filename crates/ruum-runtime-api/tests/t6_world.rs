//! The T-6 world of the runtime API (card R-3; `crates/ruum-testkit/src/gateway/`): it starts
//! `ruum-runtime-api` in process against the in-memory core and runs every runtime T-6 case,
//! then the security rules of card R-3 that the T-6 cases do not cover. The world is in
//! [`support`].

mod support;

use ruum_testkit::gateway::{self, Outcome};

/// Every runtime T-6 case with the R-3 world: none fails, none reports planned (the world is
/// there), and a case is pending only when `PENDING` lists it for a later card (G-3).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn t6_runtime_cases_pass_with_the_r3_world() {
    let cases = gateway::runtime_cases();
    let rows = gateway::run_runtime_cases(&cases, Some(support::make())).await;
    let (text, ok) = gateway::render("runtime API (R-3 world, in-memory core)", &rows);
    println!("{text}");
    assert!(ok, "{text}");
    for (name, o) in &rows {
        assert!(
            !matches!(o, Outcome::Planned(_)),
            "{name} is planned with the R-3 world: {o:?}"
        );
        match o {
            Outcome::Pending(why) => assert!(
                gateway::pending_reason(name).is_some_and(|r| r.starts_with("G-3")),
                "{name} is pending with the R-3 world, but not for G-3: {why}"
            ),
            Outcome::Passed => {}
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[path = "t6/security.rs"]
mod security;
