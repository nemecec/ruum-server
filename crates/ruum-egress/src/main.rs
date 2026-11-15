//! The `ruum-egress` binary: reads the configuration from the environment (crate README),
//! refuses to start on a bad one, and serves the proxy and control listeners.

use std::sync::Arc;

use ruum_egress::config::Config;
use ruum_egress::proxy::TracingLog;
use ruum_egress::resolver::SystemResolver;
use ruum_egress::{Options, start};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let filter = std::env::var("RUUM_EGRESS_LOG").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let config = match Config::from_vars(|name| std::env::var(name).ok()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ruum-egress: configuration error: {e}");
            std::process::exit(2);
        }
    };
    let opts = Options {
        resolver: Arc::new(SystemResolver),
        log: Arc::new(TracingLog),
        #[cfg(feature = "test-hooks")]
        test_public: None,
    };
    let running = match start(config, opts).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ruum-egress: cannot listen: {e}");
            std::process::exit(2);
        }
    };
    // `docker stop` sends SIGTERM; a terminal sends SIGINT.
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ruum-egress: cannot watch SIGTERM: {e}");
            std::process::exit(2);
        }
    };
    tokio::select! {
        r = running.proxy_task => {
            eprintln!("ruum-egress: the proxy listener stopped: {r:?}");
            std::process::exit(1);
        }
        r = running.control_task => {
            eprintln!("ruum-egress: the control listener stopped: {r:?}");
            std::process::exit(1);
        }
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
