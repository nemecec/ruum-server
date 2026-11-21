//! Workload tokens: the credential of a running app at the runtime API (card R-3;
//! `contracts/apps/runtime.openapi.yaml` "The app's credential", decision 0006-10).
//!
//! # The token
//!
//! An EdDSA JWT with `typ` `app_workload`, `aud` `ruum.internal`, `iss` the instance origin,
//! `sub` the app principal (the app node ID for the live app, the deployment ID for a preview),
//! `tid` the tenant, `app` the app node, `env` (`live` or `preview`), `iat`, `nbf`, `exp` and a
//! random `jti`. It lives [`WORKLOAD_TOKEN_LIFETIME`] (15 minutes, the contract's limit). The
//! `app` claim is an extra claim (the contract allows them): a preview principal is a deployment
//! ID, and every store read of the core names the app too.
//!
//! The key is set apart from the API keys and from the gateway's user-token key: an Ed25519
//! seed in a file (`RUUM_WORKLOAD_TOKEN_KEY_FILE`, never an environment value), with a `kid`
//! (the RFC 7638 thumbprint). The public key set ([`WorkloadTokenIssuer::jwks`]) is what the
//! runtime API reads (`RUUM_RUNTIME_WORKLOAD_JWKS_FILE`).
//!
//! The issuer is the function that the node `SpecMaker` of card I-4 calls to fill
//! `WorkloadSpec::workload_token` ([`WorkloadTokenIssuer::issue`]). A static deployment runs no
//! code and gets no token.
//!
//! # Renewal
//!
//! [`TokenRenewal`] gives each running instance a new token through
//! [`RuntimeDriver::rotate_token`] once its token is [`RENEW_EVERY`] old: one third of the
//! lifetime, so a token is valid for at most 3 renewal intervals. The Docker driver redacts only
//! the newest 16 tokens of an instance (backlog S-57), so the lifetime must stay below 15
//! intervals; the unit test `the_lifetime_stays_below_the_redaction_window` pins both.
//!
//! The renewal keeps its own list of running instances: whoever starts an instance (the deploy
//! wiring of I-4, after `RuntimeDriver::start` with a token of [`WorkloadTokenIssuer::issue`])
//! calls [`TokenRenewal::register`], and [`TokenRenewal::forget`] when it stops one. An instance
//! that the driver no longer knows is forgotten at its next renewal.
//!
//! One round ([`TokenRenewal::round`]) visits every tenant, starting after the tenant where the
//! last round started, and renews at most [`RENEW_PER_TENANT`] due instances of each tenant from
//! a rotating cursor of that tenant (backlog S-61). So a tenant with many instances never delays
//! another tenant's renewals beyond one round; only its own instances wait for a later round.
//! Until I-4 wires a driver with real sandboxes, the server runs no renewal (the tests run it
//! over the memory driver).

use crate::error::{Error, Result};
use crate::ids::{NodeId, TenantId};
use crate::runtime::{
    Environment, InstanceId, RuntimeDriver, RuntimeError, SecretValue, WorkloadIdentity,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse, ThumbprintHash};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// `typ` of a workload token.
pub const WORKLOAD_TYP: &str = "app_workload";
/// `typ` of a delegated token (`contracts/apps/delegated-token.schema.json`).
pub const DELEGATED_TYP: &str = "app_delegated";
/// `aud` of workload and delegated tokens.
pub const INTERNAL_AUDIENCE: &str = "ruum.internal";
/// How long a workload token lives (the contract: 15 minutes).
pub const WORKLOAD_TOKEN_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// How old a token is when the renewal replaces it: one third of the lifetime.
pub const RENEW_EVERY: Duration = Duration::from_secs(WORKLOAD_TOKEN_LIFETIME.as_secs() / 3);
/// How many old tokens of an instance the Docker driver keeps redacting (`MAX_KEPT_TOKENS` of
/// `ruum-runtime`, the trait doc of `rotate_token`; backlog S-57).
pub const DRIVER_KEPT_TOKENS: u32 = 16;
/// At most this many due instances of one tenant are renewed per round.
pub const RENEW_PER_TENANT: usize = 64;
/// How often the renewal runs a round when it runs on its own ([`TokenRenewal::run`]).
pub const RENEW_ROUND_EVERY: Duration = Duration::from_secs(30);

/// The PKCS#8 v1 header of an Ed25519 private key (RFC 8410); the 32-byte seed follows.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// An Ed25519 signing key with its public JWK and `kid`.
pub struct Ed25519Key {
    pub kid: String,
    pub encoding: EncodingKey,
    pub jwk: Jwk,
}

impl std::fmt::Debug for Ed25519Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ed25519Key")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl Ed25519Key {
    /// The key of a 32-byte seed.
    pub fn from_seed(seed: [u8; 32]) -> Result<Self> {
        let mut der = Vec::with_capacity(48);
        der.extend_from_slice(&ED25519_PKCS8_PREFIX);
        der.extend_from_slice(&seed);
        let encoding = EncodingKey::from_ed_der(&der);
        let mut jwk = Jwk::from_encoding_key(&encoding, Algorithm::EdDSA)
            .map_err(|e| Error::Internal(format!("an Ed25519 key: {e}")))?;
        let kid = jwk
            .thumbprint(ThumbprintHash::SHA256)
            .map_err(|e| Error::Internal(format!("an Ed25519 key: {e}")))?;
        jwk.common.key_id = Some(kid.clone());
        jwk.common.public_key_use = Some(PublicKeyUse::Signature);
        Ok(Self { kid, encoding, jwk })
    }

    /// A new random key (tests and the laptop).
    pub fn generate() -> Self {
        Self::from_seed(crate::identity::secret::random_bytes())
            .expect("a random Ed25519 seed is a valid key")
    }

    /// The key of a key file: the 32-byte seed as base64url (with or without padding and a
    /// trailing newline). `what` names the key in errors; the content is never printed.
    pub fn from_key_file(path: &std::path::Path, what: &str) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            Error::Internal(format!(
                "cannot read the {what} key file {}: {}",
                path.display(),
                e.kind()
            ))
        })?;
        let bytes = URL_SAFE_NO_PAD
            .decode(text.trim().trim_end_matches('='))
            .map_err(|_| Error::Internal(format!("the {what} key file is not base64url")))?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Internal(format!("the {what} key file must hold 32 bytes")))?;
        Self::from_seed(seed)
    }

    /// The public key set (`{"keys": [...]}`).
    pub fn jwks(&self) -> Value {
        serde_json::to_value(JwkSet {
            keys: vec![self.jwk.clone()],
        })
        .unwrap_or_else(|_| serde_json::json!({ "keys": [] }))
    }

    /// Signs `claims` (EdDSA, with the key's `kid`).
    pub fn sign<T: Serialize>(&self, claims: &T) -> Result<String> {
        let mut h = Header::new(Algorithm::EdDSA);
        h.kid = Some(self.kid.clone());
        jsonwebtoken::encode(&h, claims, &self.encoding)
            .map_err(|e| Error::Internal(format!("sign a token: {e}")))
    }
}

/// The claims of a workload token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadClaims {
    pub iss: String,
    pub aud: String,
    pub typ: String,
    /// The app principal: the app node ID (live) or the deployment ID (preview).
    pub sub: Uuid,
    pub tid: TenantId,
    /// The app node.
    pub app: NodeId,
    /// `live` or `preview`.
    pub env: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub jti: String,
}

/// The app principal of a workload: the app node ID for the live app, the deployment ID for a
/// preview (the subject of the binding tuples, card B-1).
pub fn principal_of(identity: &WorkloadIdentity) -> Uuid {
    match identity.environment {
        Environment::Live => identity.app.0,
        Environment::Preview => identity.deployment,
    }
}

/// `live` or `preview`.
pub fn env_name(e: Environment) -> &'static str {
    match e {
        Environment::Live => "live",
        Environment::Preview => "preview",
    }
}

/// Makes workload tokens (see the module docs).
pub struct WorkloadTokenIssuer {
    issuer: String,
    key: Ed25519Key,
    lifetime: Duration,
}

impl std::fmt::Debug for WorkloadTokenIssuer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkloadTokenIssuer")
            .field("issuer", &self.issuer)
            .field("kid", &self.key.kid)
            .finish_non_exhaustive()
    }
}

impl WorkloadTokenIssuer {
    /// An issuer for the instance origin `issuer` with `key`.
    pub fn new(issuer: &str, key: Ed25519Key) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            key,
            lifetime: WORKLOAD_TOKEN_LIFETIME,
        }
    }

    /// The issuer of `RUUM_WORKLOAD_TOKEN_KEY_FILE`.
    pub fn from_key_file(issuer: &str, path: &std::path::Path) -> Result<Self> {
        Ok(Self::new(
            issuer,
            Ed25519Key::from_key_file(path, "workload token")?,
        ))
    }

    /// An issuer with a new random key (tests).
    pub fn generate(issuer: &str) -> Self {
        Self::new(issuer, Ed25519Key::generate())
    }

    /// A shorter lifetime (tests of expiry). Never longer than [`WORKLOAD_TOKEN_LIFETIME`].
    pub fn with_lifetime(mut self, lifetime: Duration) -> Self {
        self.lifetime = lifetime.min(WORKLOAD_TOKEN_LIFETIME);
        self
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    pub fn kid(&self) -> &str {
        &self.key.kid
    }

    /// The public key set that the runtime API checks workload tokens against.
    pub fn jwks(&self) -> Value {
        self.key.jwks()
    }

    /// The claims of a new token of `identity`, made now.
    pub fn claims(&self, identity: &WorkloadIdentity) -> WorkloadClaims {
        let now = crate::now().unix_timestamp();
        WorkloadClaims {
            iss: self.issuer.clone(),
            aud: INTERNAL_AUDIENCE.into(),
            typ: WORKLOAD_TYP.into(),
            sub: principal_of(identity),
            tid: identity.tenant,
            app: identity.app,
            env: env_name(identity.environment).into(),
            iat: now,
            nbf: now,
            exp: now + self.lifetime.as_secs() as i64,
            jti: Uuid::now_v7().to_string(),
        }
    }

    /// Signs `claims` with the workload key (tests make tokens with other claims this way).
    pub fn sign(&self, claims: &WorkloadClaims) -> Result<SecretValue> {
        self.key.sign(claims).map(SecretValue::new)
    }

    /// A new workload token of `identity` (what the `SpecMaker` of I-4 puts into
    /// `WorkloadSpec::workload_token`, and what the renewal rotates in).
    pub fn issue(&self, identity: &WorkloadIdentity) -> Result<SecretValue> {
        self.sign(&self.claims(identity))
    }
}

// ---------------------------------------------------------------------------------- renewal

struct Entry {
    identity: WorkloadIdentity,
    /// When the instance got its current token.
    renewed_at: Instant,
}

#[derive(Default)]
struct RenewState {
    tenants: BTreeMap<TenantId, BTreeMap<InstanceId, Entry>>,
    /// The tenant after which the next round starts.
    tenant_cursor: Option<TenantId>,
    /// Per tenant: the instance after which its next visit starts.
    instance_cursor: HashMap<TenantId, InstanceId>,
}

/// What one round did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoundReport {
    pub renewed: usize,
    pub forgotten: usize,
    pub failed: usize,
}

/// Renews the workload tokens of running instances (see the module docs).
pub struct TokenRenewal {
    issuer: Arc<WorkloadTokenIssuer>,
    driver: Arc<dyn RuntimeDriver>,
    state: Mutex<RenewState>,
}

impl TokenRenewal {
    pub fn new(issuer: Arc<WorkloadTokenIssuer>, driver: Arc<dyn RuntimeDriver>) -> Self {
        Self {
            issuer,
            driver,
            state: Mutex::new(RenewState::default()),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, RenewState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records a running instance whose token was made now (after `RuntimeDriver::start`).
    pub fn register(&self, instance: InstanceId, identity: WorkloadIdentity) {
        self.register_at(instance, identity, Instant::now());
    }

    /// [`Self::register`] with the time its token was made.
    pub fn register_at(&self, instance: InstanceId, identity: WorkloadIdentity, at: Instant) {
        self.st()
            .tenants
            .entry(identity.tenant)
            .or_default()
            .insert(
                instance,
                Entry {
                    identity,
                    renewed_at: at,
                },
            );
    }

    /// Forgets a stopped instance.
    pub fn forget(&self, tenant: TenantId, instance: InstanceId) {
        let mut st = self.st();
        if let Some(t) = st.tenants.get_mut(&tenant) {
            t.remove(&instance);
            if t.is_empty() {
                st.tenants.remove(&tenant);
                st.instance_cursor.remove(&tenant);
            }
        }
    }

    /// How many instances are recorded.
    pub fn len(&self) -> usize {
        self.st().tenants.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The due instances of one round at `now`: every tenant, from the tenant cursor, at most
    /// [`RENEW_PER_TENANT`] due instances each from the tenant's own cursor.
    fn due(&self, now: Instant) -> Vec<(InstanceId, WorkloadIdentity)> {
        let mut st = self.st();
        let start = st.tenant_cursor;
        let mut order: Vec<TenantId> = st.tenants.keys().copied().collect();
        if let Some(c) = start {
            let split = order.partition_point(|t| *t <= c);
            order.rotate_left(split);
        }
        st.tenant_cursor = order.first().copied();
        let mut out = Vec::new();
        for tenant in order {
            let after = st.instance_cursor.get(&tenant).copied();
            let Some(instances) = st.tenants.get(&tenant) else {
                continue;
            };
            let mut ids: Vec<InstanceId> = instances.keys().copied().collect();
            if let Some(c) = after {
                let split = ids.partition_point(|i| *i <= c);
                ids.rotate_left(split);
            }
            let mut taken = 0;
            let mut last = None;
            for id in ids {
                if taken >= RENEW_PER_TENANT {
                    break;
                }
                let e = &instances[&id];
                if now.saturating_duration_since(e.renewed_at) >= RENEW_EVERY {
                    out.push((id, e.identity));
                    taken += 1;
                    last = Some(id);
                }
            }
            if let Some(l) = last {
                st.instance_cursor.insert(tenant, l);
            }
        }
        out
    }

    /// One round at the current time.
    pub async fn round(&self) -> RoundReport {
        self.round_at(Instant::now()).await
    }

    /// One round at `now` (see the module docs). A token that cannot be made or rotated keeps
    /// the instance for the next round; an instance the driver does not know is forgotten.
    pub async fn round_at(&self, now: Instant) -> RoundReport {
        let mut report = RoundReport::default();
        for (instance, identity) in self.due(now) {
            let token = match self.issuer.issue(&identity) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(target: "ruum::runtime", error = %e, "a workload token could not be made");
                    report.failed += 1;
                    continue;
                }
            };
            match self
                .driver
                .rotate_token(identity.tenant, instance, token)
                .await
            {
                Ok(()) => {
                    if let Some(e) = self
                        .st()
                        .tenants
                        .get_mut(&identity.tenant)
                        .and_then(|t| t.get_mut(&instance))
                    {
                        e.renewed_at = now;
                    }
                    report.renewed += 1;
                }
                Err(RuntimeError::NotFound { .. }) => {
                    self.forget(identity.tenant, instance);
                    report.forgotten += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "ruum::runtime",
                        tenant = %identity.tenant,
                        instance = %instance,
                        code = e.code(),
                        "a workload token was not renewed; the next round tries again"
                    );
                    report.failed += 1;
                }
            }
        }
        report
    }

    /// Runs a round every `every`, for ever (each round on its own task, so a panic ends one
    /// round only).
    pub async fn run(self: Arc<Self>, every: Duration) {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let me = self.clone();
            if let Err(e) = tokio::spawn(async move { me.round().await }).await {
                tracing::error!(target: "ruum::runtime", error = %e, "a renewal round panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        ImageRef, Limits, MemoryDriver, MemoryRegistry, MemoryUsage, OciDigest, ServiceSpec,
        WorkloadKind, WorkloadSpec,
    };

    /// S-57: a token is valid for at most 3 renewal intervals, never longer than 15 minutes, and
    /// well below the 16 tokens the Docker driver keeps redacting.
    #[test]
    fn the_lifetime_stays_below_the_redaction_window() {
        assert!(WORKLOAD_TOKEN_LIFETIME <= 3 * RENEW_EVERY);
        assert!(WORKLOAD_TOKEN_LIFETIME <= Duration::from_secs(15 * 60));
        assert!(WORKLOAD_TOKEN_LIFETIME < (DRIVER_KEPT_TOKENS - 1) * RENEW_EVERY);
        assert!(RENEW_ROUND_EVERY < RENEW_EVERY);
    }

    fn identity(tenant: TenantId, env: Environment) -> WorkloadIdentity {
        WorkloadIdentity {
            tenant,
            app: NodeId(Uuid::now_v7()),
            deployment: Uuid::now_v7(),
            environment: env,
        }
    }

    #[test]
    fn a_token_names_the_principal_and_lives_15_minutes() {
        let issuer = WorkloadTokenIssuer::generate("https://ruum.localhost/");
        let t = TenantId(Uuid::now_v7());
        let live = identity(t, Environment::Live);
        let c = issuer.claims(&live);
        assert_eq!(c.sub, live.app.0);
        assert_eq!(c.app, live.app);
        assert_eq!(
            (c.typ.as_str(), c.aud.as_str()),
            (WORKLOAD_TYP, INTERNAL_AUDIENCE)
        );
        assert_eq!(c.iss, "https://ruum.localhost");
        assert_eq!(c.env, "live");
        assert_eq!(c.exp - c.iat, 900);
        let prev = identity(t, Environment::Preview);
        let c = issuer.claims(&prev);
        assert_eq!(c.sub, prev.deployment);
        assert_eq!(c.env, "preview");
        assert_ne!(issuer.claims(&prev).jti, c.jti);
        // A shorter lifetime for tests, never a longer one.
        let longer =
            WorkloadTokenIssuer::generate("https://x").with_lifetime(Duration::from_secs(9999));
        let c = longer.claims(&prev);
        assert_eq!(c.exp - c.iat, 900);
    }

    #[test]
    fn the_token_verifies_with_the_public_set() {
        let issuer = WorkloadTokenIssuer::generate("https://ruum.localhost");
        let id = identity(TenantId(Uuid::now_v7()), Environment::Live);
        let token = issuer.issue(&id).unwrap();
        let jwks: JwkSet = serde_json::from_value(issuer.jwks()).unwrap();
        let header = jsonwebtoken::decode_header(token.expose()).unwrap();
        assert_eq!(header.kid.as_deref(), Some(issuer.kid()));
        let key = jsonwebtoken::DecodingKey::from_jwk(&jwks.keys[0]).unwrap();
        let mut v = jsonwebtoken::Validation::new(Algorithm::EdDSA);
        v.set_audience(&[INTERNAL_AUDIENCE]);
        let c = jsonwebtoken::decode::<WorkloadClaims>(token.expose(), &key, &v).unwrap();
        assert_eq!(c.claims.sub, id.app.0);
    }

    #[test]
    fn a_key_file_holds_a_base64url_seed() {
        let dir = std::env::temp_dir().join(format!("ruum-wl-{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        std::fs::write(&path, format!("{}\n", URL_SAFE_NO_PAD.encode([7u8; 32]))).unwrap();
        let a = WorkloadTokenIssuer::from_key_file("https://x", &path).unwrap();
        let b = WorkloadTokenIssuer::new("https://x", Ed25519Key::from_seed([7u8; 32]).unwrap());
        assert_eq!(a.kid(), b.kid());
        std::fs::write(&path, "not base64 !").unwrap();
        let e = WorkloadTokenIssuer::from_key_file("https://x", &path).unwrap_err();
        assert!(!e.to_string().contains("not base64 !"));
        std::fs::write(&path, URL_SAFE_NO_PAD.encode([7u8; 31])).unwrap();
        assert!(WorkloadTokenIssuer::from_key_file("https://x", &path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn start(
        driver: &MemoryDriver,
        reg: &MemoryRegistry,
        id: WorkloadIdentity,
        issuer: &WorkloadTokenIssuer,
    ) -> InstanceId {
        let digest = OciDigest::of(b"r3 renewal image");
        reg.push(&id, &digest);
        driver
            .start(WorkloadSpec {
                identity: id,
                kind: WorkloadKind::Service(ServiceSpec {
                    port: 3000,
                    health: None,
                    idle_timeout: None,
                }),
                image: ImageRef::Oci(digest),
                command: Vec::new(),
                env: Vec::new(),
                previous_secrets: Vec::new(),
                workload_token: issuer.issue(&id).unwrap(),
                limits: Limits {
                    cpu_millis: 100,
                    memory_bytes: 1 << 26,
                    disk_bytes: 1 << 26,
                },
                routes: Vec::new(),
            })
            .await
            .unwrap()
            .id
    }

    /// Renewal at one third of the lifetime; a fresh token is not renewed; a stopped instance
    /// that the driver forgot is forgotten.
    #[tokio::test]
    async fn due_tokens_are_renewed_and_gone_instances_forgotten() {
        let reg = MemoryRegistry::new();
        let driver = Arc::new(MemoryDriver::new(reg.clone(), Arc::new(MemoryUsage::new())));
        let issuer = Arc::new(WorkloadTokenIssuer::generate("https://x"));
        let r = TokenRenewal::new(issuer.clone(), driver.clone());
        let t = TenantId(Uuid::now_v7());
        let id = identity(t, Environment::Live);
        let inst = start(&driver, &reg, id, &issuer).await;
        let t0 = Instant::now();
        r.register_at(inst, id, t0);
        assert_eq!(
            r.round_at(t0 + RENEW_EVERY / 2).await,
            RoundReport::default()
        );
        let rep = r.round_at(t0 + RENEW_EVERY).await;
        assert_eq!(rep.renewed, 1, "{rep:?}");
        // Renewed now: not due again until another interval.
        assert_eq!(
            r.round_at(t0 + RENEW_EVERY + Duration::from_secs(1))
                .await
                .renewed,
            0
        );
        // An instance the driver never had is forgotten.
        r.register_at(
            InstanceId(Uuid::now_v7()),
            identity(t, Environment::Preview),
            t0,
        );
        let rep = r.round_at(t0 + 3 * RENEW_EVERY).await;
        assert_eq!((rep.renewed, rep.forgotten), (1, 1), "{rep:?}");
        assert_eq!(r.len(), 1);
    }

    /// S-61: a tenant with many due instances does not delay another tenant's renewal beyond
    /// one round, and its own instances are renewed in rotating pages of [`RENEW_PER_TENANT`].
    #[tokio::test]
    async fn one_tenant_never_delays_another() {
        let reg = MemoryRegistry::new();
        let driver = Arc::new(MemoryDriver::new(reg.clone(), Arc::new(MemoryUsage::new())));
        let issuer = Arc::new(WorkloadTokenIssuer::generate("https://x"));
        let r = TokenRenewal::new(issuer.clone(), driver.clone());
        let big = TenantId(Uuid::now_v7());
        let small = TenantId(Uuid::now_v7());
        let t0 = Instant::now();
        let n = RENEW_PER_TENANT * 2 + 3;
        for _ in 0..n {
            let id = identity(big, Environment::Live);
            let inst = start(&driver, &reg, id, &issuer).await;
            r.register_at(inst, id, t0);
        }
        let sid = identity(small, Environment::Live);
        let sinst = start(&driver, &reg, sid, &issuer).await;
        r.register_at(sinst, sid, t0);
        let due = t0 + RENEW_EVERY;
        let rep = r.round_at(due).await;
        assert_eq!(rep.renewed, RENEW_PER_TENANT + 1, "{rep:?}");
        {
            let st = r.st();
            assert_eq!(
                st.tenants[&small][&sinst].renewed_at, due,
                "the small tenant waited"
            );
        }
        // The big tenant goes on from its cursor: every instance within three rounds.
        let rep2 = r.round_at(due).await;
        let rep3 = r.round_at(due).await;
        assert_eq!(rep.renewed + rep2.renewed + rep3.renewed, n + 1);
        assert!(r.st().tenants[&big].values().all(|e| e.renewed_at == due));
    }
}
