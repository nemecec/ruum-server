//! Checks of the harness itself against the contract and the router.

use ruum_testkit::client::{self, fallback_fingerprint, is_unrouted};
use ruum_testkit::contract::operations;
use ruum_testkit::fixtures::{self, Who};
use ruum_testkit::requests::standard;

/// Every operation has a standard request with the method and path of the contract.
#[tokio::test]
async fn standard_requests_fit_the_contract() {
    let w = fixtures::seed(&fixtures::app_state()).await;
    let mut bad = Vec::new();
    for op in operations() {
        match standard(&op.id, &w.a) {
            None => bad.push(format!("{}: no standard request", op.id)),
            Some(r) if r.method.as_str() != op.method || !op.matches(&r.uri) => bad.push(format!(
                "{}: {} {} does not fit {} {}",
                op.id, r.method, r.uri, op.method, op.path
            )),
            Some(_) => {}
        }
    }
    assert!(bad.is_empty(), "standard requests:\n{}", bad.join("\n"));
}

/// Every operation of the contract has a route in `ruum-api` (no fallback 404, no 405).
#[tokio::test]
async fn every_operation_is_routed() {
    let state = fixtures::app_state();
    let w = fixtures::seed(&state).await;
    let app = client::app(state);
    let anyone = w.a.principal(Who::Owner);
    let fallback = fallback_fingerprint(&app, anyone).await;
    let mut missing = Vec::new();
    for op in operations() {
        let req = standard(&op.id, &w.a).expect("checked above");
        let res = client::send(&app, &req, anyone).await;
        if is_unrouted(&res, &fallback) {
            missing.push(format!("{} {} ({})", op.method, op.path, op.id));
        }
    }
    assert!(
        missing.is_empty(),
        "operations without a route:\n{}",
        missing.join("\n")
    );
}
