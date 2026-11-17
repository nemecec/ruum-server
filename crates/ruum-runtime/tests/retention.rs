//! Ended instances do not pile up (backlog R2A-5, Kimi review of run 11, finding 3): jobs that
//! start, write output and exit, one after another on the real engine. The driver keeps at most
//! `max_ended_instances` ended ones with their log rings, so the instances and log bytes it holds
//! stay bounded however many jobs ran. The policy itself (retention, read logs, byte cap, a full
//! driver) has unit tests in `src/driver.rs`.

mod common;

use common::{Harness, engine_or_skip, new_usage};
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::program::{self, Step};
use ruum_core::runtime::{
    Environment, InstanceState, JobSpec, Limits, RuntimeDriver, RuntimeError, SecretValue,
    WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use std::time::{Duration, Instant};

const KEPT: usize = 3;
const RUNS: usize = 10;
const LINES: usize = 200;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_jobs_keep_the_driver_bounded() {
    if !engine_or_skip("retention") {
        return;
    }
    let h = Harness::start();
    let mut cfg = h.config();
    cfg.max_ended_instances = KEPT;
    let d = h
        .driver_with(
            cfg,
            new_usage(),
            std::sync::Arc::new(ruum_runtime::NoEgress),
        )
        .await;
    let tenant = TenantId::new();
    let app = NodeId::new();
    let line = "y".repeat(512);
    let steps: Vec<Step> = (0..LINES)
        .map(|_| Step::Print(line.clone()))
        .chain([Step::Exit(0)])
        .collect();
    let mut ids = Vec::new();
    let mut most_bytes = 0;
    for run in 0..RUNS {
        let identity = WorkloadIdentity {
            tenant,
            app,
            deployment: uuid::Uuid::now_v7(),
            environment: Environment::Live,
        };
        let spec = WorkloadSpec {
            identity,
            kind: WorkloadKind::Job(JobSpec {
                timeout: Duration::from_secs(60),
            }),
            image: h.push(&identity).expect("fixture image"),
            command: program::command(&steps),
            env: Vec::new(),
            previous_secrets: Vec::new(),
            workload_token: SecretValue::new(format!("wt_{}", uuid::Uuid::now_v7().simple())),
            limits: Limits {
                cpu_millis: 1000,
                memory_bytes: 128 * 1024 * 1024,
                disk_bytes: 16 * 1024 * 1024,
            },
            routes: Vec::new(),
        };
        let s = d.start(spec).await.expect("the job starts");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let state = d.status(tenant, s.id).await.expect("status").state;
            if let InstanceState::Exited(e) = state {
                assert_eq!(e.code, Some(0), "run {run}: {e:?}");
                break;
            }
            assert!(
                Instant::now() < deadline,
                "run {run}: never exited: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let logs = d.logs(tenant, s.id, None, 10 * LINES).await.expect("logs");
        assert_eq!(logs.len(), LINES, "run {run}: every line in the ring");
        ids.push(s.id);
        let kept = d.kept_instances();
        assert!(kept <= KEPT + 1, "run {run}: {kept} instances kept");
        most_bytes = most_bytes.max(d.kept_log_bytes());
    }
    // Each kept ring holds LINES lines of about 512 bytes: the bytes stay at KEPT + 1 rings.
    let ring = LINES * (512 + 256);
    assert!(
        most_bytes <= (KEPT + 1) * ring,
        "{most_bytes} log bytes kept after {RUNS} jobs"
    );
    assert!(
        matches!(
            d.status(tenant, ids[0]).await,
            Err(RuntimeError::NotFound { .. })
        ),
        "the first job is forgotten"
    );
    let last = *ids.last().unwrap();
    assert_eq!(
        d.logs(tenant, last, None, 10 * LINES).await.unwrap().len(),
        LINES,
        "the last job's logs are still there"
    );
}
