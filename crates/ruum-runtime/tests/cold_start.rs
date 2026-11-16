//! Cold start (decision 25; card R-2a acceptance 3): a small Node service that scaled to zero
//! answers its health check within 3 s after `wake`. 20 wakes; the test prints the median and the
//! p95 and fails when the p95 is over 3 s. Run it with `RUUM_RUNTIME_DRIVER=docker` and `runsc`.

mod common;

use common::{Harness, engine_or_skip, new_usage, show};
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::program::{self, Step};
use ruum_core::runtime::{
    Environment, Health, HealthCheck, InstanceState, Limits, RuntimeDriver, SecretValue,
    ServiceSpec, WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use std::time::{Duration, Instant};

const WAKES: usize = 20;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopped_service_answers_within_three_seconds() {
    if !engine_or_skip("cold start") {
        return;
    }
    let harness = Harness::start();
    let driver = harness.driver(new_usage()).await;
    let identity = WorkloadIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: uuid::Uuid::now_v7(),
        environment: Environment::Live,
    };
    let image = harness.push(&identity).expect("fixture image");
    let spec = WorkloadSpec {
        identity,
        kind: WorkloadKind::Service(ServiceSpec {
            port: 8080,
            health: Some(HealthCheck {
                path: "/health".to_owned(),
                interval: Duration::from_secs(1),
                failure_threshold: 3,
            }),
            idle_timeout: Some(Duration::from_secs(1)),
        }),
        image,
        command: program::command(&[Step::Serve]),
        env: Vec::new(),
        previous_secrets: Vec::new(),
        workload_token: SecretValue::new(format!("wt_{}", uuid::Uuid::now_v7().simple())),
        limits: Limits {
            cpu_millis: 1000,
            memory_bytes: 256 * 1024 * 1024,
            disk_bytes: 64 * 1024 * 1024,
        },
        routes: Vec::new(),
    };
    let s = driver.start(spec).await.expect("start");
    let tenant = identity.tenant;
    let mut times = Vec::new();
    for i in 0..WAKES {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let st = driver.status(tenant, s.id).await.expect("status");
            if st.state == InstanceState::ScaledToZero {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "wake {i}: never scaled to zero: {:?}",
                st.state
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let t0 = Instant::now();
        let woke = driver.wake(tenant, s.id).await.expect("wake");
        let took = t0.elapsed();
        assert!(
            woke.state == InstanceState::Running && woke.health == Health::Healthy,
            "wake {i}: {:?} {:?}",
            woke.state,
            woke.health
        );
        times.push(took);
    }
    let _ = driver.stop(tenant, s.id).await;
    times.sort();
    let median = times[WAKES / 2];
    let p95 = times[((WAKES as f64 * 0.95).ceil() as usize).min(WAKES) - 1];
    show(&format!(
        "cold start [{}]: {WAKES} wakes, median {:.3} s, p95 {:.3} s, min {:.3} s, max {:.3} s",
        driver.name(),
        median.as_secs_f64(),
        p95.as_secs_f64(),
        times[0].as_secs_f64(),
        times[WAKES - 1].as_secs_f64()
    ));
    assert!(p95 <= Duration::from_secs(3), "p95 {p95:?} is over 3 s");
}
