//! The egress proxy (decision 23, decision 0006 answer 5 and section G). See the crate README.
//!
//! - [`api`]: the control interface types (the seam with the runtime driver, card R-2a).
//! - [`policy`]: the `EgressPolicy` module border and its in-memory implementation.
//! - [`addr`]: the address rules, `EGRESS_DENY`, host parsing.
//! - [`proxy`]: the `CONNECT` listener; [`control`]: the control listener.
//! - [`conformance`]: the conformance suite (test builds only).

pub mod addr;
pub mod allow;
pub mod api;
pub mod config;
pub mod control;
pub mod limits;
pub mod policy;
pub mod proxy;
pub mod resolver;
pub mod usage;

#[cfg(feature = "test-hooks")]
pub mod conformance;

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::{TcpListener, UnixListener};
use tokio::task::JoinHandle;

use crate::config::{Config, ControlAddr};
use crate::control::ControlState;
use crate::policy::MemoryPolicy;
use crate::proxy::{LogSink, ProxyState};
use crate::resolver::Resolver;
use crate::usage::Usage;

/// What [`start`] needs besides the configuration. The binary passes the system resolver and the
/// `tracing` log; the conformance suite passes fakes.
pub struct Options {
    pub resolver: Arc<dyn Resolver>,
    pub log: Arc<dyn LogSink>,
    /// Test builds only: one loopback socket address treated as public.
    #[cfg(feature = "test-hooks")]
    pub test_public: Option<SocketAddr>,
}

/// A started proxy.
pub struct Running {
    /// The proxy listener's address (the real port when the configuration asked for port 0).
    pub proxy_addr: SocketAddr,
    pub control: ControlAddr,
    pub policy: Arc<MemoryPolicy>,
    pub usage: Arc<Usage>,
    pub proxy_task: JoinHandle<()>,
    pub control_task: JoinHandle<()>,
}

impl Running {
    pub fn abort(&self) {
        self.proxy_task.abort();
        self.control_task.abort();
    }
}

/// Binds both listeners and serves them. The binary's `main` and the suite both start here.
pub async fn start(config: Config, opts: Options) -> std::io::Result<Running> {
    let policy = Arc::new(MemoryPolicy::new(
        config.deny.clone(),
        config.ports.clone(),
        config.limits,
        config.max_networks,
    ));
    #[cfg(feature = "test-hooks")]
    if let Some(a) = opts.test_public {
        policy.set_test_public(a);
    }
    let usage = Arc::new(Usage::new());

    let control_state = ControlState {
        policy: policy.clone(),
        usage: usage.clone(),
        token: config.token.clone(),
    };
    let app = control::router(control_state);
    let control_task = match &config.control {
        ControlAddr::Unix(path) => {
            // A stale socket of an earlier run would stop the bind.
            if std::fs::symlink_metadata(path)
                .map(|m| {
                    use std::os::unix::fs::FileTypeExt;
                    m.file_type().is_socket()
                })
                .unwrap_or(false)
            {
                std::fs::remove_file(path)?;
            }
            let l = UnixListener::bind(path)?;
            tokio::spawn(async move {
                if let Err(e) = axum::serve(l, app).await {
                    tracing::error!("egress control listener failed: {e}");
                }
            })
        }
        ControlAddr::Tcp(addr) => {
            let l = TcpListener::bind(addr).await?;
            tokio::spawn(async move {
                if let Err(e) = axum::serve(l, app).await {
                    tracing::error!("egress control listener failed: {e}");
                }
            })
        }
    };

    let listener = TcpListener::bind(config.listen).await?;
    let proxy_addr = listener.local_addr()?;
    let state = Arc::new(ProxyState {
        policy: policy.clone(),
        resolver: opts.resolver,
        usage: usage.clone(),
        log: opts.log,
        connect_timeout: config.connect_timeout,
        head_timeout: config.head_timeout,
        unknown: ProxyState::unknown_slots(),
        unknown_rate: ProxyState::unknown_rate(),
    });
    let max = config.max_connections;
    let proxy_task = tokio::spawn(proxy::serve(listener, state, max));

    tracing::info!(
        listen = %proxy_addr,
        control = %config.control,
        ports = ?config.ports,
        deny_entries = config.deny.entries().len(),
        dev = config.dev,
        "ruum-egress started"
    );
    if config.dev {
        tracing::warn!(
            "ruum-egress runs in dev mode (RUUM_EGRESS_DEV=1): EGRESS_DENY may be empty, so the host's own public addresses are not refused"
        );
    }
    Ok(Running {
        proxy_addr,
        control: config.control,
        policy,
        usage,
        proxy_task,
        control_task,
    })
}
