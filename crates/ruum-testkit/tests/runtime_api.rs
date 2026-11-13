//! The runtime API cases of card T-6 (`crates/ruum-testkit/src/gateway/cases_runtime_api.rs`):
//! without a world every case reports planned (cards R-3 and G-3 implement `RuntimeApiWorld`),
//! and every operation of `contracts/apps/runtime.openapi.yaml` is named by a case.

use ruum_testkit::contract;
use ruum_testkit::gateway::{self, Outcome, RUNTIME_CONTRACT};
use std::collections::BTreeSet;

#[tokio::test]
async fn without_a_world_every_runtime_case_is_planned() {
    let cases = gateway::runtime_cases();
    let rows = gateway::run_runtime_cases(&cases, None).await;
    let (text, ok) = gateway::render("runtime API (no world)", &rows);
    println!("{text}");
    assert!(ok, "{text}");
    for (name, o) in &rows {
        assert!(matches!(o, Outcome::Planned(_)), "{name}: {o:?}");
    }
}

#[test]
fn every_runtime_operation_is_named_by_a_case() {
    let ops = contract::parse(RUNTIME_CONTRACT).expect("the runtime contract scans");
    assert!(!ops.is_empty());
    let named: BTreeSet<&str> = gateway::runtime_cases()
        .iter()
        .flat_map(|c| c.ops.iter().copied())
        .collect();
    let ids: BTreeSet<&str> = ops.iter().map(|o| o.id.as_str()).collect();
    for op in &ops {
        assert!(
            named.contains(op.id.as_str()),
            "no runtime API case names {} ({} {})",
            op.id,
            op.method,
            op.path
        );
    }
    for n in &named {
        assert!(
            ids.contains(n),
            "a runtime case names {n}, which is not in runtime.openapi.yaml"
        );
    }
}
