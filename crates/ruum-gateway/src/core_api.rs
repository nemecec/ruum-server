//! The small internal API of the core that the gateway asks (decision 0006-1; the routes are in
//! `ruum-api`'s internal listener, the types in `ruum_core::identity::app_sign_in::wire`).
//!
//! [`CoreApi`] is the module border: [`HttpCore`] talks to the core's internal listener
//! (`RUUM_GATEWAY_INTERNAL_URL`), tests use any other implementation. [`CoreCache`] keeps the
//! answers that the access check needs for at most [`CHECK_TTL`] per visitor and app, so a
//! removed right or role, a sign-out of Ruum and a withdrawn public approval stop working within
//! 5 seconds (card G-1a). Errors fail closed: the request gets the gateway's `503` page.

use async_trait::async_trait;
use ruum_core::TenantId;
use ruum_core::identity::app_sign_in::AppBinding;
use ruum_core::identity::app_sign_in::wire::{
    AgentAnswer, AgentRequest, DeploymentInfo, DeploymentQuery, EndSessionRequest, PREFIX,
    RedeemRequest, RedeemResponse, SessionInfo, SessionRequest,
};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long the gateway keeps an answer of the access check (at most 5 s, card G-1a).
pub const CHECK_TTL: Duration = Duration::from_secs(3);
/// How long it keeps a refusal (a cookie or token that the core does not know).
pub const REFUSAL_TTL: Duration = Duration::from_secs(1);
/// The most cached answers of each kind.
pub const CACHE_MAX: usize = 50_000;

/// An error of the core API (the request fails closed).
#[derive(Debug)]
pub struct CoreError(pub String);

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the core API failed: {}", self.0)
    }
}

pub type CoreResult<T> = Result<T, CoreError>;

/// The questions the gateway asks the core.
#[async_trait]
pub trait CoreApi: Send + Sync {
    async fn deployment(&self, q: &DeploymentQuery) -> CoreResult<Option<DeploymentInfo>>;
    async fn redeem(&self, r: &RedeemRequest) -> CoreResult<Option<RedeemResponse>>;
    async fn session(&self, r: &SessionRequest) -> CoreResult<Option<SessionInfo>>;
    async fn end_session(&self, r: &EndSessionRequest) -> CoreResult<()>;
    async fn agent(&self, r: &AgentRequest) -> CoreResult<AgentAnswer>;
}

/// The core's internal listener over HTTP.
pub struct HttpCore {
    base: String,
    client: reqwest::Client,
}

impl HttpCore {
    /// A client of the internal listener at `base` (`http://ruum-server:8081`).
    pub fn new(base: &str) -> CoreResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            client,
        })
    }

    async fn post<B: serde::Serialize + Sync>(
        &self,
        path: &str,
        body: &B,
    ) -> CoreResult<reqwest::Response> {
        self.client
            .post(format!("{}{PREFIX}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| CoreError(format!("{path}: {}", e.without_url())))
    }

    async fn json_or_none<T: serde::de::DeserializeOwned>(
        path: &str,
        r: reqwest::Response,
        none: reqwest::StatusCode,
    ) -> CoreResult<Option<T>> {
        let status = r.status();
        if status == none {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(CoreError(format!("{path}: status {status}")));
        }
        r.json::<T>()
            .await
            .map(Some)
            .map_err(|e| CoreError(format!("{path}: {}", e.without_url())))
    }
}

#[async_trait]
impl CoreApi for HttpCore {
    async fn deployment(&self, q: &DeploymentQuery) -> CoreResult<Option<DeploymentInfo>> {
        let r = self.post("/deployment", q).await?;
        Self::json_or_none("/deployment", r, reqwest::StatusCode::NOT_FOUND).await
    }

    async fn redeem(&self, q: &RedeemRequest) -> CoreResult<Option<RedeemResponse>> {
        let r = self.post("/redeem", q).await?;
        Self::json_or_none("/redeem", r, reqwest::StatusCode::BAD_REQUEST).await
    }

    async fn session(&self, q: &SessionRequest) -> CoreResult<Option<SessionInfo>> {
        let r = self.post("/session", q).await?;
        Self::json_or_none("/session", r, reqwest::StatusCode::NOT_FOUND).await
    }

    async fn end_session(&self, q: &EndSessionRequest) -> CoreResult<()> {
        let r = self.post("/session/end", q).await?;
        if r.status().is_success() {
            Ok(())
        } else {
            Err(CoreError(format!("/session/end: status {}", r.status())))
        }
    }

    async fn agent(&self, q: &AgentRequest) -> CoreResult<AgentAnswer> {
        let r = self.post("/agent", q).await?;
        Self::json_or_none("/agent", r, reqwest::StatusCode::NOT_FOUND)
            .await?
            .ok_or_else(|| CoreError("/agent: no answer".into()))
    }
}

/// A key of a credential in the caches: a hash, never the value itself.
pub fn credential_key(value: &str) -> [u8; 32] {
    *blake3::hash(value.as_bytes()).as_bytes()
}

struct Timed<V> {
    at: Instant,
    ttl: Duration,
    value: V,
}

/// A small map whose entries live for a bounded time.
struct TtlMap<K, V> {
    entries: Mutex<HashMap<K, Timed<V>>>,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> TtlMap<K, V> {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, k: &K) -> Option<V> {
        let e = self.entries.lock().expect("cache lock");
        e.get(k)
            .filter(|t| t.at.elapsed() < t.ttl)
            .map(|t| t.value.clone())
    }

    fn put(&self, k: K, v: V, ttl: Duration) {
        let mut e = self.entries.lock().expect("cache lock");
        if e.len() >= CACHE_MAX {
            e.retain(|_, t| t.at.elapsed() < t.ttl);
            if e.len() >= CACHE_MAX {
                e.clear();
            }
        }
        e.insert(
            k,
            Timed {
                at: Instant::now(),
                ttl: ttl.min(Duration::from_secs(5)),
                value: v,
            },
        );
    }

    fn retain(&self, f: impl Fn(&K) -> bool) {
        self.entries.lock().expect("cache lock").retain(|k, _| f(k));
    }
}

/// [`CoreApi`] with the short caches of the access check (see the module docs).
pub struct CoreCache {
    inner: std::sync::Arc<dyn CoreApi>,
    deployments: TtlMap<(TenantId, uuid::Uuid), Option<DeploymentInfo>>,
    sessions: TtlMap<([u8; 32], AppBinding), Option<SessionInfo>>,
    agents: TtlMap<([u8; 32], AppBinding), AgentAnswer>,
}

impl CoreCache {
    pub fn new(inner: std::sync::Arc<dyn CoreApi>) -> Self {
        Self {
            inner,
            deployments: TtlMap::new(),
            sessions: TtlMap::new(),
            agents: TtlMap::new(),
        }
    }

    /// The core API without the caches.
    pub fn inner(&self) -> &dyn CoreApi {
        self.inner.as_ref()
    }

    pub async fn deployment(&self, q: &DeploymentQuery) -> CoreResult<Option<DeploymentInfo>> {
        let k = (q.tenant_id, q.deployment_id);
        if let Some(v) = self.deployments.get(&k) {
            return Ok(v);
        }
        let v = self.inner.deployment(q).await?;
        self.deployments.put(k, v.clone(), CHECK_TTL);
        Ok(v)
    }

    pub async fn session(
        &self,
        binding: &AppBinding,
        secret: &str,
    ) -> CoreResult<Option<SessionInfo>> {
        let k = (credential_key(secret), *binding);
        if let Some(v) = self.sessions.get(&k) {
            return Ok(v);
        }
        let v = self
            .inner
            .session(&SessionRequest {
                binding: *binding,
                session: secret.to_string(),
            })
            .await?;
        let ttl = if v.is_some() { CHECK_TTL } else { REFUSAL_TTL };
        self.sessions.put(k, v.clone(), ttl);
        Ok(v)
    }

    pub async fn agent(&self, binding: &AppBinding, token: &str) -> CoreResult<AgentAnswer> {
        let k = (credential_key(token), *binding);
        if let Some(v) = self.agents.get(&k) {
            return Ok(v);
        }
        let v = self
            .inner
            .agent(&AgentRequest {
                binding: *binding,
                token: token.to_string(),
            })
            .await?;
        self.agents.put(k, v.clone(), CHECK_TTL);
        Ok(v)
    }

    pub async fn redeem(&self, r: &RedeemRequest) -> CoreResult<Option<RedeemResponse>> {
        self.inner.redeem(r).await
    }

    /// Ends a session in the core and forgets its cached answers.
    pub async fn end_session(&self, tenant: TenantId, secret: &str) -> CoreResult<()> {
        let key = credential_key(secret);
        self.sessions.retain(|(k, _)| *k != key);
        self.inner
            .end_session(&EndSessionRequest {
                tenant_id: tenant,
                session: secret.to_string(),
            })
            .await?;
        self.sessions.retain(|(k, _)| *k != key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::NodeId;
    use ruum_core::identity::app_sign_in::AppEnv;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counting {
        sessions: AtomicUsize,
        allowed: std::sync::atomic::AtomicBool,
    }

    #[async_trait]
    impl CoreApi for Counting {
        async fn deployment(&self, _: &DeploymentQuery) -> CoreResult<Option<DeploymentInfo>> {
            Ok(Some(DeploymentInfo {
                environment: AppEnv::Live,
                access: "platform".into(),
                public_approved: false,
                serving: true,
            }))
        }
        async fn redeem(&self, _: &RedeemRequest) -> CoreResult<Option<RedeemResponse>> {
            Ok(None)
        }
        async fn session(&self, _: &SessionRequest) -> CoreResult<Option<SessionInfo>> {
            self.sessions.fetch_add(1, Ordering::SeqCst);
            Ok(Some(SessionInfo {
                sub: "pairwise:x".into(),
                allowed: self.allowed.load(Ordering::SeqCst),
                roles: vec![],
            }))
        }
        async fn end_session(&self, _: &EndSessionRequest) -> CoreResult<()> {
            Ok(())
        }
        async fn agent(&self, _: &AgentRequest) -> CoreResult<AgentAnswer> {
            Ok(AgentAnswer::Foreign)
        }
    }

    #[tokio::test]
    async fn session_answers_are_cached_per_credential_and_binding_and_forgotten_at_logout() {
        let core = std::sync::Arc::new(Counting::default());
        core.allowed.store(true, Ordering::SeqCst);
        let c = CoreCache::new(core.clone());
        let b = AppBinding::live(TenantId::new(), NodeId::new());
        let other = AppBinding::live(b.tenant_id, NodeId::new());
        assert!(c.session(&b, "s1").await.unwrap().unwrap().allowed);
        c.session(&b, "s1").await.unwrap();
        assert_eq!(core.sessions.load(Ordering::SeqCst), 1);
        c.session(&other, "s1").await.unwrap();
        c.session(&b, "s2").await.unwrap();
        assert_eq!(core.sessions.load(Ordering::SeqCst), 3);
        c.end_session(b.tenant_id, "s1").await.unwrap();
        c.session(&b, "s1").await.unwrap();
        assert_eq!(core.sessions.load(Ordering::SeqCst), 4);
        assert!(CHECK_TTL <= Duration::from_secs(5));
    }
}
