//! A server restart (card R-2a review, change 2; design 12.7): every release starts a new
//! server while the networks of the old one are still there. A new driver adopts the network of
//! an app and environment when its labels name that app and it reads back as the driver makes it
//! (and registers it at the egress proxy again); a network of the same name that is not the
//! driver's is refused. The full reconciliation at `connect` (orphaned containers, lost usage)
//! is backlog R2A-1.

mod common;

use async_trait::async_trait;
use common::{Harness, LABEL, docker, engine_or_skip, new_usage};
use ruum_core::ids::{NodeId, TenantId};
use ruum_core::runtime::program::{self, Step};
use ruum_core::runtime::{
    Environment, InstanceState, Limits, Route, RuntimeDriver, RuntimeError, SecretValue,
    ServiceSpec, WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use ruum_runtime::{EgressNetwork, EgressRegistry, network_name};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Records the registrations instead of calling a proxy.
#[derive(Default)]
struct Recording {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl EgressRegistry for Recording {
    async fn register(&self, n: &EgressNetwork) -> Result<(), RuntimeError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("put {}", n.network));
        Ok(())
    }
    async fn unregister(&self, network: &str) -> Result<(), RuntimeError> {
        self.calls.lock().unwrap().push(format!("delete {network}"));
        Ok(())
    }
}

fn identity() -> WorkloadIdentity {
    WorkloadIdentity {
        tenant: TenantId::new(),
        app: NodeId::new(),
        deployment: uuid::Uuid::now_v7(),
        environment: Environment::Live,
    }
}

fn spec(h: &Harness, identity: WorkloadIdentity) -> WorkloadSpec {
    WorkloadSpec {
        identity,
        kind: WorkloadKind::Service(ServiceSpec {
            port: 8080,
            health: None,
            idle_timeout: None,
        }),
        image: h.push(&identity).expect("fixture image"),
        command: program::command(&[Step::Serve]),
        env: Vec::new(),
        previous_secrets: Vec::new(),
        workload_token: SecretValue::new(format!("wt_{}", uuid::Uuid::now_v7().simple())),
        limits: Limits {
            cpu_millis: 500,
            memory_bytes: 128 * 1024 * 1024,
            disk_bytes: 16 * 1024 * 1024,
        },
        routes: vec![Route::EgressProxy, Route::InternalGateway],
    }
}

async fn wait_running(d: &dyn RuntimeDriver, tenant: TenantId, id: ruum_core::runtime::InstanceId) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let s = d.status(tenant, id).await.expect("status").state;
        if s == InstanceState::Running {
            return;
        }
        assert!(Instant::now() < deadline, "never running: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_driver_adopts_the_network_of_the_last_one() {
    if !engine_or_skip("restart") {
        return;
    }
    let h = Harness::start();
    let rec = Arc::new(Recording::default());
    let id = identity();

    // The old server starts the app and ends without stopping it (a release).
    let first = h.driver_with(h.config(), new_usage(), rec.clone()).await;
    let a = first.start(spec(&h, id)).await.expect("the first start");
    wait_running(first.as_ref(), id.tenant, a.id).await;
    drop(first);

    // The new server starts the same app again on the same network.
    let second = h.driver_with(h.config(), new_usage(), rec.clone()).await;
    let b = second
        .start(spec(&h, id))
        .await
        .expect("the new driver starts the app whose network is still there");
    wait_running(second.as_ref(), id.tenant, b.id).await;
    assert_eq!(a.applied.network, b.applied.network, "the same network");
    let calls = rec.calls.lock().unwrap().clone();
    let puts: Vec<&String> = calls.iter().filter(|c| c.starts_with("put ")).collect();
    assert_eq!(puts.len(), 2, "registered by both drivers: {calls:?}");
    assert_eq!(puts[0], puts[1], "the same network ID: {calls:?}");
    second.stop(id.tenant, b.id).await.expect("stop");

    // A network of the app's name that the driver did not make is refused, not used.
    let other = identity();
    let name = network_name(&other);
    docker(&["network", "create", "--internal", "--label", LABEL, &name])
        .expect("a foreign network");
    match second.start(spec(&h, other)).await {
        Err(RuntimeError::Internal { reason }) => {
            assert!(reason.contains("not the driver's"), "{reason}");
        }
        r => panic!("a foreign network of the app's name was used: {r:?}"),
    }
}
