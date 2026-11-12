//! The drain at shutdown (design 12.7; backlog O-25).
//!
//! Kubernetes stops a pod with SIGTERM, after the pod's `preStop` pause has taken it out of the
//! Service. On SIGTERM (or Ctrl-C) every listener stops taking new connections, idle keep-alive
//! connections close, and open requests (long polls, downloads, WebDAV transfers) may finish
//! during the grace time (`RUUM_SHUTDOWN_GRACE_SECONDS`, default 35: longer than the longest
//! long poll, 30 seconds). Connections still open at the end of the grace time are closed when
//! the process exits; clients retry against another replica.

use std::future::Future;
use std::time::Duration;
use tokio::sync::watch;

/// The default grace time. The chart's `terminationGracePeriodSeconds` must be longer than the
/// `preStop` pause plus this time.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(35);

/// A stop signal that many listeners can wait on.
#[derive(Clone)]
pub struct Stop(watch::Receiver<bool>);

/// The sending side of [`Stop`].
pub struct StopHandle(watch::Sender<bool>);

pub fn stop_pair() -> (StopHandle, Stop) {
    let (tx, rx) = watch::channel(false);
    (StopHandle(tx), Stop(rx))
}

impl StopHandle {
    pub fn stop(&self) {
        let _ = self.0.send(true);
    }
}

impl Stop {
    /// Resolves once [`StopHandle::stop`] was called (or the handle is gone).
    pub async fn stopped(mut self) {
        let _ = self.0.wait_for(|stopped| *stopped).await;
    }
}

/// Resolves on SIGTERM or SIGINT (Ctrl-C).
pub async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "no SIGTERM handler; only Ctrl-C stops the server");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// How the servers ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Drained {
    /// The servers ended by themselves, without a stop.
    Ended,
    /// After the stop, every open connection finished within the grace time.
    Clean,
    /// The grace time ran out with connections still open.
    TimedOut,
}

/// Runs `servers` (all listeners, each with a graceful shutdown on `stop`) until they end. Once
/// `stop` resolves, waits at most `grace` for them to finish.
pub async fn drain<S>(servers: S, stop: Stop, grace: Duration) -> std::io::Result<Drained>
where
    S: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(servers);
    tokio::select! {
        result = &mut servers => return result.map(|()| Drained::Ended),
        () = stop.stopped() => {}
    }
    match tokio::time::timeout(grace, &mut servers).await {
        Ok(result) => result.map(|()| Drained::Clean),
        Err(_) => Ok(Drained::TimedOut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};

    async fn server(
        delay: Duration,
        stop: Stop,
    ) -> (
        std::net::SocketAddr,
        impl Future<Output = std::io::Result<()>>,
    ) {
        let app = axum::Router::new().route(
            "/slow",
            get(move || async move {
                tokio::time::sleep(delay).await;
                "done"
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serve =
            axum::serve(listener, app.into_make_service()).with_graceful_shutdown(stop.stopped());
        (addr, async move { serve.await })
    }

    async fn get_slow(addr: std::net::SocketAddr) -> String {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(b"GET /slow HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out).await;
        out
    }

    /// An open request finishes after the stop; a new connection is refused.
    #[tokio::test]
    async fn an_open_request_finishes_during_the_drain() {
        let (handle, stop) = stop_pair();
        let (addr, serve) = server(Duration::from_millis(400), stop.clone()).await;
        let drained = tokio::spawn(drain(serve, stop, Duration::from_secs(5)));
        let request = tokio::spawn(get_slow(addr));
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.stop();
        let answer = request.await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.ends_with("done"), "{answer}");
        assert_eq!(drained.await.unwrap().unwrap(), Drained::Clean);
        assert!(TcpStream::connect(addr).await.is_err(), "still accepting");
    }

    /// A request longer than the grace time does not keep the process alive.
    #[tokio::test]
    async fn the_grace_time_ends_the_drain() {
        let (handle, stop) = stop_pair();
        let (addr, serve) = server(Duration::from_secs(30), stop.clone()).await;
        let drained = tokio::spawn(drain(serve, stop, Duration::from_millis(300)));
        let _request = tokio::spawn(get_slow(addr));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        handle.stop();
        assert_eq!(drained.await.unwrap().unwrap(), Drained::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Without a stop, the result of the servers passes through.
    #[tokio::test]
    async fn servers_that_end_by_themselves() {
        let (_h, stop) = stop_pair();
        let r = drain(async { Ok(()) }, stop, Duration::from_secs(1)).await;
        assert_eq!(r.unwrap(), Drained::Ended);
    }
}
