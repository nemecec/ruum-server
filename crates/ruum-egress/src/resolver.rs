//! Name resolution behind a small trait, so the conformance suite can give answers that change
//! between calls (the DNS rebinding case).

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::IpAddr;
use std::sync::Mutex;

use async_trait::async_trait;

#[async_trait]
pub trait Resolver: Send + Sync {
    /// Every address of a name. The proxy calls it once per request.
    async fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>>;
}

/// The system resolver (`getaddrinfo` through tokio).
pub struct SystemResolver;

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        let addrs = tokio::net::lookup_host((name, 0)).await?;
        Ok(addrs.map(|a| a.ip()).collect())
    }
}

/// A resolver with answers set by a test. Each name has a sequence of answers: a call takes the
/// first one, and the last one stays. A name without answers does not resolve.
#[derive(Default)]
pub struct FakeResolver {
    inner: Mutex<FakeInner>,
}

#[derive(Default)]
struct FakeInner {
    answers: HashMap<String, VecDeque<Vec<IpAddr>>>,
    calls: HashMap<String, usize>,
}

impl FakeResolver {
    pub fn new() -> FakeResolver {
        FakeResolver::default()
    }

    /// Sets the answers of `name` (lowercase, no trailing dot), in order.
    pub fn set(&self, name: &str, answers: Vec<Vec<IpAddr>>) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.answers.insert(name.to_string(), answers.into());
    }

    /// How often `name` was resolved.
    pub fn calls(&self, name: &str) -> usize {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.calls.get(name).copied().unwrap_or(0)
    }
}

#[async_trait]
impl Resolver for FakeResolver {
    async fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        *g.calls.entry(name.to_string()).or_default() += 1;
        let Some(q) = g.answers.get_mut(name) else {
            return Err(io::Error::new(io::ErrorKind::NotFound, "no such name"));
        };
        let answer = if q.len() > 1 {
            q.pop_front().unwrap_or_default()
        } else {
            q.front().cloned().unwrap_or_default()
        };
        Ok(answer)
    }
}
