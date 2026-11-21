//! The small internal API of the core that the runtime API asks (decision 0006-1; the routes
//! are in `ruum-api`'s internal listener, the types in `ruum_core::apps::runtime_api::wire`).
//!
//! [`CoreApi`] is the module border: [`HttpCore`] talks to the core's internal listener
//! (`RUUM_RUNTIME_INTERNAL_URL`). Every call carries the app's own token (`token`) as
//! `Authorization: Bearer`: the core checks it again and takes the caller only from it, so the
//! body's caller is a claim the core compares, never trusts (review of R-3, required change 1). [`PrincipalCache`] keeps the core's answer about a principal
//! for at most [`CHECK_TTL`], so a stopped preview, a trashed app or a deployment that is no
//! longer live stops working within 5 seconds. Errors fail closed (`503`).

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use ruum_core::TenantId;
use ruum_core::apps::runtime_api::wire::{
    Caller, ChildPage, ExchangeAnswer, ExchangeQuery, HoldersAnswer, HoldersQuery, PREFIX,
    PrincipalAnswer, ReadQuery, Refusal, TopicAnswer, TopicQuery,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long the runtime API keeps the core's answer about a principal (at most 5 s).
pub const CHECK_TTL: Duration = Duration::from_secs(3);
/// Kept principal answers per tenant.
pub const PRINCIPALS_PER_TENANT: usize = 1024;
/// Kept principal answers in all; over it, the tenant with the most entries loses its oldest.
pub const PRINCIPALS_MAX: usize = 50_000;

/// An error of the core API (the request fails closed).
#[derive(Debug)]
pub struct CoreError(pub String);

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the core API failed: {}", self.0)
    }
}

pub type CoreResult<T> = Result<T, CoreError>;

/// The content of a bound file, as a stream.
pub struct CoreContent {
    pub length: Option<u64>,
    pub body: BoxStream<'static, Result<Bytes, std::io::Error>>,
}

/// The questions the runtime API asks the core.
#[async_trait]
pub trait CoreApi: Send + Sync {
    async fn principal(&self, token: &str, c: &Caller) -> CoreResult<PrincipalAnswer>;
    async fn holders(&self, token: &str, q: &HoldersQuery) -> CoreResult<HoldersAnswer>;
    async fn exchange(&self, token: &str, q: &ExchangeQuery) -> CoreResult<ExchangeAnswer>;
    async fn content(&self, token: &str, q: &ReadQuery)
    -> CoreResult<Result<CoreContent, Refusal>>;
    async fn children(&self, token: &str, q: &ReadQuery) -> CoreResult<Result<ChildPage, Refusal>>;
    async fn topic(&self, token: &str, q: &TopicQuery) -> CoreResult<TopicAnswer>;
}

/// The core's internal listener over HTTP.
pub struct HttpCore {
    base: String,
    client: reqwest::Client,
    /// For content: no total timeout, a timeout per read instead.
    streams: reqwest::Client,
}

impl HttpCore {
    /// A client of the internal listener at `base` (`http://ruum-server:8081`).
    pub fn new(base: &str) -> CoreResult<Self> {
        // Never through a proxy of the environment (`HTTP_PROXY`, `ALL_PROXY`): every call
        // carries the app's token, and the answers carry delegated tokens and bound content
        // (Kimi review of run 14, R-3 finding 1).
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        let streams = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            client,
            streams,
        })
    }

    async fn post<B: serde::Serialize + Sync>(
        &self,
        client: &reqwest::Client,
        path: &str,
        token: &str,
        body: &B,
    ) -> CoreResult<reqwest::Response> {
        client
            .post(format!("{}{PREFIX}{path}", self.base))
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .map_err(|e| CoreError(format!("{path}: {}", e.without_url())))
    }

    async fn json<T: serde::de::DeserializeOwned, B: serde::Serialize + Sync>(
        &self,
        path: &str,
        token: &str,
        body: &B,
    ) -> CoreResult<T> {
        let r = self.post(&self.client, path, token, body).await?;
        if !r.status().is_success() {
            return Err(CoreError(format!("{path}: status {}", r.status())));
        }
        r.json::<T>()
            .await
            .map_err(|e| CoreError(format!("{path}: {}", e.without_url())))
    }

    /// A refusal of a read route, from its status.
    fn refusal(path: &str, status: reqwest::StatusCode) -> CoreResult<Refusal> {
        match status.as_u16() {
            404 => Ok(Refusal::NotFound),
            403 => Ok(Refusal::Denied),
            410 => Ok(Refusal::Gone),
            400 => Ok(Refusal::BadCursor),
            s => Err(CoreError(format!("{path}: status {s}"))),
        }
    }
}

#[async_trait]
impl CoreApi for HttpCore {
    async fn principal(&self, token: &str, c: &Caller) -> CoreResult<PrincipalAnswer> {
        self.json("/principal", token, c).await
    }

    async fn holders(&self, token: &str, q: &HoldersQuery) -> CoreResult<HoldersAnswer> {
        self.json("/holders", token, q).await
    }

    async fn exchange(&self, token: &str, q: &ExchangeQuery) -> CoreResult<ExchangeAnswer> {
        self.json("/exchange", token, q).await
    }

    async fn content(
        &self,
        token: &str,
        q: &ReadQuery,
    ) -> CoreResult<Result<CoreContent, Refusal>> {
        use futures::StreamExt;
        let r = self.post(&self.streams, "/content", token, q).await?;
        if !r.status().is_success() {
            return Self::refusal("/content", r.status()).map(Err);
        }
        let length = r.content_length();
        let body = r
            .bytes_stream()
            .map(|c| c.map_err(|e| std::io::Error::other(e.without_url())))
            .boxed();
        Ok(Ok(CoreContent { length, body }))
    }

    async fn children(&self, token: &str, q: &ReadQuery) -> CoreResult<Result<ChildPage, Refusal>> {
        let r = self.post(&self.client, "/children", token, q).await?;
        if !r.status().is_success() {
            return Self::refusal("/children", r.status()).map(Err);
        }
        r.json::<ChildPage>()
            .await
            .map(Ok)
            .map_err(|e| CoreError(format!("/children: {}", e.without_url())))
    }

    async fn topic(&self, token: &str, q: &TopicQuery) -> CoreResult<TopicAnswer> {
        self.json("/topic", token, q).await
    }
}

/// The core's answers about principals, each for at most [`CHECK_TTL`]. Per tenant at most
/// [`PRINCIPALS_PER_TENANT`]; in all at most [`PRINCIPALS_MAX`], and when that is reached the
/// tenant with the most entries loses its oldest first (backlog M-59: one tenant's many apps
/// never push another tenant's answers out).
#[derive(Default)]
pub struct PrincipalCache {
    inner: Mutex<Principals>,
}

#[derive(Default)]
struct Principals {
    by_tenant: HashMap<TenantId, HashMap<Caller, (PrincipalAnswer, Instant)>>,
    total: usize,
}

impl Principals {
    fn evict_oldest(&mut self, tenant: TenantId) {
        let Some(m) = self.by_tenant.get_mut(&tenant) else {
            return;
        };
        if let Some(k) = m
            .iter()
            .min_by_key(|(_, (_, at))| *at)
            .map(|(k, _)| k.clone())
        {
            m.remove(&k);
            self.total -= 1;
        }
        if m.is_empty() {
            self.by_tenant.remove(&tenant);
        }
    }
}

impl PrincipalCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, Principals> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A kept answer that is younger than [`CHECK_TTL`].
    pub fn get(&self, c: &Caller, now: Instant) -> Option<PrincipalAnswer> {
        self.lock()
            .by_tenant
            .get(&c.tenant_id)?
            .get(c)
            .filter(|(_, at)| now.saturating_duration_since(*at) < CHECK_TTL)
            .map(|(a, _)| a.clone())
    }

    pub fn put(&self, c: &Caller, a: PrincipalAnswer, now: Instant) {
        let mut p = self.lock();
        let tenant = c.tenant_id;
        if p.by_tenant.get(&tenant).map_or(0, HashMap::len) >= PRINCIPALS_PER_TENANT {
            if let Some(m) = p.by_tenant.get_mut(&tenant) {
                let before = m.len();
                m.retain(|_, (_, at)| now.saturating_duration_since(*at) < CHECK_TTL);
                let gone = before - m.len();
                p.total -= gone;
            }
            if p.by_tenant.get(&tenant).map_or(0, HashMap::len) >= PRINCIPALS_PER_TENANT {
                p.evict_oldest(tenant);
            }
        }
        while p.total >= PRINCIPALS_MAX {
            let Some(largest) = p
                .by_tenant
                .iter()
                .max_by_key(|(_, m)| m.len())
                .map(|(t, _)| *t)
            else {
                break;
            };
            p.evict_oldest(largest);
        }
        if p.by_tenant
            .entry(tenant)
            .or_default()
            .insert(c.clone(), (a, now))
            .is_none()
        {
            p.total += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.lock().total
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::NodeId;
    use ruum_core::identity::app_sign_in::AppEnv;
    use uuid::Uuid;

    fn caller(t: TenantId) -> Caller {
        let app = NodeId(Uuid::now_v7());
        Caller {
            tenant_id: t,
            app_id: app,
            principal: app.0,
            deployment_id: Uuid::now_v7(),
            environment: AppEnv::Live,
        }
    }

    #[test]
    fn an_answer_lives_at_most_the_ttl() {
        let p = PrincipalCache::default();
        let c = caller(TenantId(Uuid::now_v7()));
        let now = Instant::now();
        p.put(&c, PrincipalAnswer::Gone, now);
        assert_eq!(p.get(&c, now), Some(PrincipalAnswer::Gone));
        assert_eq!(p.get(&c, now + CHECK_TTL), None);
        assert!(CHECK_TTL <= Duration::from_secs(5));
    }

    #[test]
    fn one_tenant_cannot_push_out_another() {
        let p = PrincipalCache::default();
        let a = TenantId(Uuid::now_v7());
        let b = TenantId(Uuid::now_v7());
        let now = Instant::now();
        let cb = caller(b);
        p.put(&cb, PrincipalAnswer::Gone, now);
        for i in 0..PRINCIPALS_PER_TENANT + 5 {
            p.put(
                &caller(a),
                PrincipalAnswer::Gone,
                now + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(p.len(), PRINCIPALS_PER_TENANT + 1);
        assert!(p.get(&cb, now).is_some());
    }
}
