//! `ruum_core::runtime::conformance::run`, unchanged, against the Docker driver (card R-2a,
//! acceptance 1). `RUUM_RUNTIME_DRIVER=docker` (default) runs it with runc,
//! `RUUM_RUNTIME_DRIVER=runsc` with gVisor: then the engine must have the runtime `runsc`, or
//! the driver does not start and the test fails (never skips).

mod common;

use async_trait::async_trait;
use common::{Harness, engine_or_skip, new_usage};
use ruum_core::runtime::conformance::{self, DriverFixture, Timing};
use ruum_core::runtime::{ImageRef, MemoryUsage, RuntimeDriver, WorkloadIdentity};
use std::sync::Arc;
use std::time::Duration;

struct Docker {
    harness: Harness,
    driver: Arc<dyn RuntimeDriver>,
    usage: Arc<MemoryUsage>,
}

#[async_trait]
impl DriverFixture for Docker {
    fn driver(&self) -> Arc<dyn RuntimeDriver> {
        self.driver.clone()
    }
    fn usage(&self) -> Arc<MemoryUsage> {
        self.usage.clone()
    }
    async fn program_image(&self, identity: &WorkloadIdentity) -> Result<ImageRef, String> {
        self.harness.push(identity)
    }
    fn timing(&self) -> Timing {
        Timing {
            idle_timeout: Duration::from_secs(4),
            health_interval: Duration::from_millis(500),
            job_timeout: Duration::from_secs(3),
            wait: Duration::from_secs(45),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_docker_driver_passes_the_conformance_suite() {
    if !engine_or_skip("conformance") {
        return;
    }
    let harness = Harness::start();
    let usage = new_usage();
    let driver = harness.driver(usage.clone()).await;
    let f = Docker {
        harness,
        driver,
        usage,
    };
    let report = conformance::run(&f).await;
    println!("{}", report.summary());
    report.assert_ok();
}
