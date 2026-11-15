//! The conformance suite against the in-memory policy (card R-2e). Linux only: the cases use
//! loopback source and upstream addresses other than 127.0.0.1.
#![cfg(target_os = "linux")]

use ruum_egress::conformance::{case_startup_checks, memory_harness};

ruum_egress::conformance_tests!(memory_harness);

#[test]
fn startup_checks() {
    case_startup_checks();
}
