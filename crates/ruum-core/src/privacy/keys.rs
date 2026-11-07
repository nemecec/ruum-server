//! The tenant pseudonym key (decision 0004-16, card Q-4).
//!
//! Each tenant has one random 32-byte pseudonym key in `tenant_keys` (migration 0004). The
//! table holds it only WRAPPED: AES-256-GCM under a key-encryption key (KEK) behind the
//! [`KeyWrapper`] trait:
//!
//! - [`OpenBaoTransitWrapper`] in production: OpenBao transit `encrypt` / `decrypt` with the
//!   transit key `ruum-tenant-keys`. The KEK never leaves OpenBao.
//! - [`LocalKekWrapper`] on the laptop and in tests: a file of 32 random bytes
//!   (`RUUM_LOCAL_KEK_FILE`; `scripts/dev.sh up` makes it, `dev.sh clean` removes it with the
//!   platform database volume), because dev OpenBao keeps its data in memory only.
//!
//! [`TenantKeys`] makes the key on first use (the first database of the tenant): a random key,
//! wrapped, then `insert ... on conflict do nothing` and a read of the stored row, so two racing
//! first uses keep one key (the primary key decides). The key is never rotated in step 2:
//! `key_version` is always 1.
//!
//! [`TenantKeys::database_key`] gives the pseudonym key of one database: the tenant key when
//! `databases.key_node_id` is NULL, else the key derived for that node (decision 0004-25,
//! [`PseudonymKey::for_database`]). The engine gets it when it creates or copies a database or
//! opens a SQLite connection (decision 0004-30, card D-1). The only plain copies of a key are
//! `ruum_mask.key` inside the engine databases and the per-connection SQLite registration; no
//! platform table and no log holds one (the tests here check both).
//!
//! A tenant export (design section 14) carries the key re-wrapped for the target cell
//! ([`TenantKeys::export_for`]), and the import in the target cell stores it under the target's
//! KEK ([`TenantKeys::import`]), so the pseudonyms stay the same after a move. The tenant export
//! pipeline does not exist yet; it must call these two functions when it is built.
//!
//! Every wrap binds the tenant: the additional authenticated data of AES-GCM (and of OpenBao
//! transit) is `ruum-tenant-key:<tenant>:pseudonym:1`. A wrapped key copied to the row of
//! another tenant does not unwrap.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use super::PseudonymKey;
use crate::error::{Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId};
use crate::journal::postgres::begin_tenant;

/// `tenant_keys.purpose` of the pseudonym key.
pub const PURPOSE_PSEUDONYM: &str = "pseudonym";
/// `tenant_keys.key_version`. The key is never rotated in step 2 (decision 0004-16).
pub const KEY_VERSION: i32 = 1;
/// The OpenBao transit key that wraps the tenant keys (migration 0004, design section 4).
pub const OPENBAO_TRANSIT_KEY: &str = "ruum-tenant-keys";
/// The default mount path of the OpenBao transit engine.
pub const OPENBAO_TRANSIT_MOUNT: &str = "transit";
/// The variable with the path of the local KEK file.
pub const LOCAL_KEK_FILE_ENV: &str = "RUUM_LOCAL_KEK_FILE";
/// The variables of the OpenBao transit wrapper.
pub const OPENBAO_ADDR_ENV: &str = "RUUM_OPENBAO_ADDR";
pub const OPENBAO_TOKEN_ENV: &str = "RUUM_OPENBAO_TOKEN";
pub const OPENBAO_MOUNT_ENV: &str = "RUUM_OPENBAO_TRANSIT_MOUNT";

/// The length of the local KEK and of an AES-GCM nonce.
const KEK_LEN: usize = 32;
const NONCE_LEN: usize = 12;

// ------------------------------------------------------------------------- key comparison

// review-Q-0 note 5: the comparison of two keys is constant-time. `privacy/mod.rs` no longer
// derives `PartialEq` for `PseudonymKey`; this is its only implementation.
impl PartialEq for PseudonymKey {
    fn eq(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.as_bytes().iter().zip(other.as_bytes()) {
            diff |= a ^ b;
        }
        std::hint::black_box(diff) == 0
    }
}

impl Eq for PseudonymKey {}

// ------------------------------------------------------------------------------ wrapped key

/// A tenant key wrapped by a KEK: `tenant_keys.wrapped_key` and `tenant_keys.kek_id`. Not
/// secret, but `Debug` shows only the KEK and the length.
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedKey {
    /// The KEK and its version, for example `local:3f2a9c0d1b7e4a55` or
    /// `openbao-transit:transit/ruum-tenant-keys:v1`.
    pub kek_id: String,
    /// The local wrapper: nonce (12 bytes) || ciphertext (32) || tag (16), 60 bytes. OpenBao:
    /// the transit ciphertext `vault:v<n>:<base64>` as UTF-8 (89 bytes for version 1).
    pub bytes: Vec<u8>,
}

impl fmt::Debug for WrappedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WrappedKey")
            .field("kek_id", &self.kek_id)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// The additional authenticated data of a wrap: binds the wrapped key to its tenant, purpose
/// and version.
fn aad(tenant: TenantId) -> String {
    format!("ruum-tenant-key:{tenant}:{PURPOSE_PSEUDONYM}:{KEY_VERSION}")
}

fn key_error(what: &str) -> Error {
    Error::Internal(format!("tenant key: {what}"))
}

/// A random 32-byte key.
fn random_key() -> PseudonymKey {
    let mut bytes = [0u8; PseudonymKey::LEN];
    rand::fill(&mut bytes[..]);
    let key = PseudonymKey::from_bytes(bytes);
    bytes.fill(0);
    std::hint::black_box(&bytes);
    key
}

// ----------------------------------------------------------------------------- key wrapper

/// The key-encryption key. It wraps and unwraps tenant keys and never gives out its own key.
#[async_trait]
pub trait KeyWrapper: Send + Sync {
    /// The KEK without its version (OpenBao adds the version of the transit key to the
    /// `kek_id` of each [`WrappedKey`]). A wrapped key whose `kek_id` is not this one, or this
    /// one followed by `:` and a version, does not unwrap here.
    fn kek_id(&self) -> &str;

    /// Wraps the key of `tenant`.
    async fn wrap(&self, tenant: TenantId, key: &PseudonymKey) -> Result<WrappedKey>;

    /// Unwraps a key that [`KeyWrapper::wrap`] made for `tenant` with this KEK.
    async fn unwrap(&self, tenant: TenantId, wrapped: &WrappedKey) -> Result<PseudonymKey>;
}

/// Checks that a wrapped key belongs to the KEK `kek_id` (exactly, or with a version suffix).
fn check_kek(kek_id: &str, wrapped: &WrappedKey) -> Result<()> {
    let ok = wrapped.kek_id == kek_id
        || wrapped
            .kek_id
            .strip_prefix(kek_id)
            .is_some_and(|rest| rest.starts_with(':'));
    if ok {
        Ok(())
    } else {
        Err(key_error(&format!(
            "the key is wrapped by {}, not by {kek_id}",
            wrapped.kek_id
        )))
    }
}

/// The KEK from the environment: OpenBao transit when `RUUM_OPENBAO_ADDR` is set (with
/// `RUUM_OPENBAO_TOKEN`, optional `RUUM_OPENBAO_TRANSIT_MOUNT`), else the local KEK file of
/// `RUUM_LOCAL_KEK_FILE`.
pub fn wrapper_from_env() -> Result<Arc<dyn KeyWrapper>> {
    if let Some(addr) = std::env::var(OPENBAO_ADDR_ENV)
        .ok()
        .filter(|v| !v.is_empty())
    {
        let token = std::env::var(OPENBAO_TOKEN_ENV).map_err(|_| {
            key_error(&format!(
                "{OPENBAO_ADDR_ENV} is set but {OPENBAO_TOKEN_ENV} is not"
            ))
        })?;
        let mount = std::env::var(OPENBAO_MOUNT_ENV)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| OPENBAO_TRANSIT_MOUNT.to_string());
        return Ok(Arc::new(OpenBaoTransitWrapper::new(
            &addr,
            token,
            &mount,
            OPENBAO_TRANSIT_KEY,
        )?));
    }
    match std::env::var(LOCAL_KEK_FILE_ENV)
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(path) => Ok(Arc::new(LocalKekWrapper::open(path)?)),
        None => Err(key_error(&format!(
            "no key-encryption key: set {OPENBAO_ADDR_ENV} or {LOCAL_KEK_FILE_ENV} \
             (eval \"$(scripts/dev.sh env)\" on the laptop)"
        ))),
    }
}

/// The local KEK: a file of exactly 32 random bytes. AES-256-GCM with a random 96-bit nonce per
/// wrap; the wrapped key is nonce || ciphertext || tag (60 bytes).
pub struct LocalKekWrapper {
    cipher: Aes256Gcm,
    kek_id: String,
}

impl fmt::Debug for LocalKekWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalKekWrapper")
            .field("kek_id", &self.kek_id)
            .finish_non_exhaustive()
    }
}

impl LocalKekWrapper {
    /// Reads the KEK file. It must hold exactly 32 bytes. The error never shows its content.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut bytes = std::fs::read(path)
            .map_err(|e| key_error(&format!("cannot read the KEK file {}: {e}", path.display())))?;
        let kek = <[u8; KEK_LEN]>::try_from(bytes.as_slice()).map_err(|_| {
            key_error(&format!(
                "the KEK file {} must hold exactly {KEK_LEN} bytes",
                path.display()
            ))
        });
        bytes.fill(0);
        std::hint::black_box(&bytes);
        let mut kek = kek?;
        let wrapper = Self::from_bytes(&kek);
        kek.fill(0);
        std::hint::black_box(&kek);
        Ok(wrapper)
    }

    /// A wrapper on a KEK in memory (tests).
    pub fn from_bytes(kek: &[u8; KEK_LEN]) -> Self {
        // The ID names the KEK without showing it: 16 hex of SHA-256 over a fixed label and the
        // KEK. An import with another KEK file fails with a clear error instead of a tag error.
        let mut h = Sha256::new();
        h.update(b"ruum-local-kek-id:");
        h.update(kek);
        let digest = h.finalize();
        LocalKekWrapper {
            cipher: Aes256Gcm::new(kek.into()),
            kek_id: format!("local:{}", hex::encode(&digest[..8])),
        }
    }

    /// Writes a new KEK file of 32 random bytes (mode 0600 on Unix). Refuses to overwrite an
    /// existing file: a lost KEK loses every tenant key it wraps.
    pub fn create_file(path: impl AsRef<Path>) -> Result<()> {
        use std::io::Write as _;
        let path = path.as_ref();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(path).map_err(|e| {
            key_error(&format!(
                "cannot create the KEK file {}: {e}",
                path.display()
            ))
        })?;
        let mut kek = [0u8; KEK_LEN];
        rand::fill(&mut kek[..]);
        let written = file.write_all(&kek).and_then(|()| file.sync_all());
        kek.fill(0);
        std::hint::black_box(&kek);
        written.map_err(|e| {
            key_error(&format!(
                "cannot write the KEK file {}: {e}",
                path.display()
            ))
        })
    }
}

#[async_trait]
impl KeyWrapper for LocalKekWrapper {
    fn kek_id(&self) -> &str {
        &self.kek_id
    }

    async fn wrap(&self, tenant: TenantId, key: &PseudonymKey) -> Result<WrappedKey> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::fill(&mut nonce[..]);
        let aad = aad(tenant);
        let sealed = self
            .cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: key.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| key_error("AES-GCM encryption failed"))?;
        let mut bytes = Vec::with_capacity(NONCE_LEN + sealed.len());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);
        Ok(WrappedKey {
            kek_id: self.kek_id.clone(),
            bytes,
        })
    }

    async fn unwrap(&self, tenant: TenantId, wrapped: &WrappedKey) -> Result<PseudonymKey> {
        check_kek(&self.kek_id, wrapped)?;
        if wrapped.bytes.len() != NONCE_LEN + PseudonymKey::LEN + 16 {
            return Err(key_error("a locally wrapped key has 60 bytes"));
        }
        let (nonce, sealed) = wrapped.bytes.split_at(NONCE_LEN);
        let nonce = <[u8; NONCE_LEN]>::try_from(nonce).expect("12 bytes");
        let aad = aad(tenant);
        let mut plain = self
            .cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: sealed,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| key_error("the wrapped key does not unwrap with this KEK and tenant"))?;
        let key = PseudonymKey::from_slice(&plain);
        plain.fill(0);
        std::hint::black_box(&plain);
        key.ok_or_else(|| key_error("the unwrapped key is not 32 bytes long"))
    }
}

/// The production KEK: OpenBao transit (`POST /v1/<mount>/encrypt/<key>` and `.../decrypt/...`,
/// key type `aes256-gcm96`). The token needs `update` on both paths. The `kek_id` of a wrapped
/// key is `openbao-transit:<mount>/<key>:v<version>`.
pub struct OpenBaoTransitWrapper {
    http: reqwest::Client,
    base: String,
    token: String,
    mount: String,
    key: String,
    kek_id: String,
}

impl fmt::Debug for OpenBaoTransitWrapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenBaoTransitWrapper")
            .field("base", &self.base)
            .field("kek_id", &self.kek_id)
            .finish_non_exhaustive()
    }
}

#[derive(serde::Deserialize)]
struct TransitAnswer {
    data: TransitData,
}

#[derive(serde::Deserialize)]
struct TransitData {
    #[serde(default)]
    ciphertext: Option<String>,
    #[serde(default)]
    plaintext: Option<String>,
}

impl OpenBaoTransitWrapper {
    /// `addr`: for example `http://127.0.0.1:8200`. `mount`: the transit mount (`transit`).
    pub fn new(addr: &str, token: String, mount: &str, key: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| key_error(&format!("OpenBao client: {e}")))?;
        let mount = mount.trim_matches('/').to_string();
        Ok(OpenBaoTransitWrapper {
            http,
            base: addr.trim_end_matches('/').to_string(),
            token,
            kek_id: format!("openbao-transit:{mount}/{key}"),
            mount,
            key: key.to_string(),
        })
    }

    async fn call(&self, op: &str, body: serde_json::Value) -> Result<TransitData> {
        let url = format!("{}/v1/{}/{op}/{}", self.base, self.mount, self.key);
        let res = self
            .http
            .post(&url)
            .header("X-Vault-Token", &self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| key_error(&format!("OpenBao transit {op}: {e}")))?;
        let status = res.status();
        if !status.is_success() {
            // The body is not shown: only the status.
            return Err(key_error(&format!(
                "OpenBao transit {op} answered {status}"
            )));
        }
        let answer: TransitAnswer = res
            .json()
            .await
            .map_err(|_| key_error(&format!("OpenBao transit {op}: unexpected answer")))?;
        Ok(answer.data)
    }
}

#[async_trait]
impl KeyWrapper for OpenBaoTransitWrapper {
    fn kek_id(&self) -> &str {
        &self.kek_id
    }

    async fn wrap(&self, tenant: TenantId, key: &PseudonymKey) -> Result<WrappedKey> {
        let body = serde_json::json!({
            "plaintext": B64.encode(key.as_bytes()),
            "associated_data": B64.encode(aad(tenant)),
        });
        let data = self.call("encrypt", body).await?;
        let ciphertext = data
            .ciphertext
            .ok_or_else(|| key_error("OpenBao transit encrypt gave no ciphertext"))?;
        // `vault:v<n>:<base64>`: the version of the transit key goes into the kek_id.
        let version = ciphertext
            .strip_prefix("vault:v")
            .and_then(|rest| rest.split_once(':'))
            .map(|(v, _)| v)
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| key_error("OpenBao transit ciphertext has no version"))?;
        Ok(WrappedKey {
            kek_id: format!("{}:v{version}", self.kek_id),
            bytes: ciphertext.into_bytes(),
        })
    }

    async fn unwrap(&self, tenant: TenantId, wrapped: &WrappedKey) -> Result<PseudonymKey> {
        check_kek(&self.kek_id, wrapped)?;
        let ciphertext = std::str::from_utf8(&wrapped.bytes)
            .map_err(|_| key_error("an OpenBao wrapped key is UTF-8 text"))?;
        let body = serde_json::json!({
            "ciphertext": ciphertext,
            "associated_data": B64.encode(aad(tenant)),
        });
        let data = self.call("decrypt", body).await?;
        let mut plaintext = data
            .plaintext
            .ok_or_else(|| key_error("OpenBao transit decrypt gave no plaintext"))?
            .into_bytes();
        let decoded = B64.decode(&plaintext);
        // Best effort: overwrite the base64 text of the key.
        plaintext.fill(0);
        std::hint::black_box(&plaintext);
        let mut bytes =
            decoded.map_err(|_| key_error("OpenBao transit plaintext is not base64"))?;
        let key = PseudonymKey::from_slice(&bytes);
        bytes.fill(0);
        std::hint::black_box(&bytes);
        key.ok_or_else(|| key_error("the unwrapped key is not 32 bytes long"))
    }
}

// ------------------------------------------------------------------------------- key store

/// A row of `tenant_keys` (purpose `pseudonym`).
#[derive(Clone, Debug)]
pub struct StoredKey {
    pub tenant: TenantId,
    pub key_version: i32,
    pub wrapped: WrappedKey,
    pub created_at: OffsetDateTime,
}

/// The storage of `tenant_keys`. Every call reads or writes only the rows of `tenant`.
#[async_trait]
pub trait TenantKeyStore: Send + Sync {
    /// The pseudonym key of the tenant (version 1), if it has one.
    async fn get(&self, tenant: TenantId) -> Result<Option<StoredKey>>;

    /// Stores `wrapped` as the pseudonym key of the tenant (version 1) unless the tenant has
    /// one already, and returns the stored row: the new one, or the one that was there (the
    /// winner of a race). Never replaces a key.
    async fn insert_if_missing(&self, tenant: TenantId, wrapped: &WrappedKey) -> Result<StoredKey>;
}

/// [`TenantKeyStore`] in memory (unit tests).
#[derive(Debug, Default)]
pub struct MemoryTenantKeyStore {
    rows: Mutex<HashMap<TenantId, StoredKey>>,
}

impl MemoryTenantKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TenantKeyStore for MemoryTenantKeyStore {
    async fn get(&self, tenant: TenantId) -> Result<Option<StoredKey>> {
        Ok(self.rows.lock().expect("lock").get(&tenant).cloned())
    }

    async fn insert_if_missing(&self, tenant: TenantId, wrapped: &WrappedKey) -> Result<StoredKey> {
        let mut rows = self.rows.lock().expect("lock");
        let row = rows.entry(tenant).or_insert_with(|| StoredKey {
            tenant,
            key_version: KEY_VERSION,
            wrapped: wrapped.clone(),
            created_at: OffsetDateTime::now_utc(),
        });
        Ok(row.clone())
    }
}

/// [`TenantKeyStore`] on `tenant_keys`. Each call runs in its own transaction with
/// `ruum.tenant_id` set (row-level security), and every query also filters by `tenant_id`.
#[derive(Clone, Debug)]
pub struct PgTenantKeyStore {
    pool: PgPool,
}

impl PgTenantKeyStore {
    pub fn new(pool: PgPool) -> Self {
        PgTenantKeyStore { pool }
    }
}

const SELECT_KEY: &str = "select wrapped_key, kek_id, key_version, created_at from tenant_keys \
     where tenant_id = $1 and purpose = $2 and key_version = $3";

fn stored_from_row(tenant: TenantId, row: &sqlx::postgres::PgRow) -> Result<StoredKey> {
    Ok(StoredKey {
        tenant,
        key_version: row.try_get("key_version")?,
        wrapped: WrappedKey {
            kek_id: row.try_get("kek_id")?,
            bytes: row.try_get("wrapped_key")?,
        },
        created_at: row.try_get("created_at")?,
    })
}

#[async_trait]
impl TenantKeyStore for PgTenantKeyStore {
    async fn get(&self, tenant: TenantId) -> Result<Option<StoredKey>> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        let row = sqlx::query(SELECT_KEY)
            .bind(tenant.0)
            .bind(PURPOSE_PSEUDONYM)
            .bind(KEY_VERSION)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        row.map(|r| stored_from_row(tenant, &r)).transpose()
    }

    async fn insert_if_missing(&self, tenant: TenantId, wrapped: &WrappedKey) -> Result<StoredKey> {
        let mut tx = begin_tenant(&self.pool, tenant).await?;
        // A racing insert of the same tenant waits for the other transaction; after its commit
        // this one does nothing, and the read below (a new snapshot under READ COMMITTED) sees
        // the winner's row.
        sqlx::query(
            "insert into tenant_keys (tenant_id, purpose, key_version, wrapped_key, kek_id) \
             values ($1, $2, $3, $4, $5) on conflict (tenant_id, purpose, key_version) do nothing",
        )
        .bind(tenant.0)
        .bind(PURPOSE_PSEUDONYM)
        .bind(KEY_VERSION)
        .bind(&wrapped.bytes)
        .bind(&wrapped.kek_id)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(SELECT_KEY)
            .bind(tenant.0)
            .bind(PURPOSE_PSEUDONYM)
            .bind(KEY_VERSION)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| key_error("the key row is missing after the insert"))?;
        tx.commit().await?;
        stored_from_row(tenant, &row)
    }
}

// ------------------------------------------------------------------------------ tenant keys

/// A tenant key wrapped for another cell (design section 14): what a tenant export carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedKey {
    pub tenant: TenantId,
    pub purpose: String,
    pub key_version: i32,
    pub wrapped: WrappedKey,
}

/// The tenant keys of one cell: the store and the KEK.
#[derive(Clone)]
pub struct TenantKeys {
    store: Arc<dyn TenantKeyStore>,
    wrapper: Arc<dyn KeyWrapper>,
}

impl fmt::Debug for TenantKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantKeys")
            .field("kek_id", &self.wrapper.kek_id())
            .finish_non_exhaustive()
    }
}

impl TenantKeys {
    pub fn new(store: Arc<dyn TenantKeyStore>, wrapper: Arc<dyn KeyWrapper>) -> Self {
        TenantKeys { store, wrapper }
    }

    /// The pseudonym key of the tenant. The first call makes it: a random key, wrapped and
    /// stored unless another call stored one first; every call then returns the stored key.
    pub async fn tenant_key(&self, tenant: TenantId) -> Result<PseudonymKey> {
        if let Some(stored) = self.store.get(tenant).await? {
            return self.unwrap_stored(tenant, &stored).await;
        }
        let key = random_key();
        let wrapped = self.wrapper.wrap(tenant, &key).await?;
        let stored = self.store.insert_if_missing(tenant, &wrapped).await?;
        if stored.wrapped == wrapped {
            tracing::info!(
                tenant = %tenant,
                kek_id = %wrapped.kek_id,
                "made the pseudonym key of the tenant"
            );
            Ok(key)
        } else {
            // Another first use won the race: its key counts.
            self.unwrap_stored(tenant, &stored).await
        }
    }

    /// The pseudonym key of a database (decision 0004-25): the tenant key when
    /// `databases.key_node_id` is NULL, else the key derived for that node. Makes the tenant key
    /// on first use. The key prints as `PseudonymKey(<hidden>)` and does not serialize.
    pub async fn database_key(
        &self,
        tenant: TenantId,
        key_node_id: Option<NodeId>,
    ) -> Result<PseudonymKey> {
        let tenant_key = self.tenant_key(tenant).await?;
        Ok(PseudonymKey::for_database(&tenant_key, key_node_id))
    }

    /// The pseudonym key of a database as the engine's handle (decision 0004-30): the key of
    /// [`TenantKeys::database_key`], handed over inside this module, so no caller outside
    /// `privacy` touches the key bytes. The engine installs it and never gives it back.
    /// `createDatabase` calls it with `key_node_id: None` (the tenant key).
    pub async fn engine_key(
        &self,
        tenant: TenantId,
        key_node_id: Option<NodeId>,
    ) -> Result<ruum_dbengine::PseudonymKey> {
        let key = self.database_key(tenant, key_node_id).await?;
        Ok(ruum_dbengine::PseudonymKey::from_bytes(*key.as_bytes()))
    }

    /// The tenant key re-wrapped for the KEK of another cell, for the tenant export of design
    /// section 14. `None` when the tenant has no key yet (it never had a database); the export
    /// then carries none, and the target makes one on first use.
    pub async fn export_for(
        &self,
        tenant: TenantId,
        target: &dyn KeyWrapper,
    ) -> Result<Option<ExportedKey>> {
        let Some(stored) = self.store.get(tenant).await? else {
            return Ok(None);
        };
        let key = self.unwrap_stored(tenant, &stored).await?;
        let wrapped = target.wrap(tenant, &key).await?;
        Ok(Some(ExportedKey {
            tenant,
            purpose: PURPOSE_PSEUDONYM.to_string(),
            key_version: stored.key_version,
            wrapped,
        }))
    }

    /// Stores an exported key in this cell (the tenant import of design section 14). The key
    /// must be wrapped for this cell's KEK. Importing the same key again is a no-op; a tenant
    /// that has another key already gives `conflict` (a key is never replaced).
    pub async fn import(&self, exported: &ExportedKey) -> Result<()> {
        if exported.purpose != PURPOSE_PSEUDONYM || exported.key_version != KEY_VERSION {
            return Err(Error::api(
                ErrorCode::ValidationFailed,
                "only the pseudonym key of version 1 can be imported",
            ));
        }
        let tenant = exported.tenant;
        let key = self.wrapper.unwrap(tenant, &exported.wrapped).await?;
        let stored = self
            .store
            .insert_if_missing(tenant, &exported.wrapped)
            .await?;
        if stored.wrapped == exported.wrapped || self.unwrap_stored(tenant, &stored).await? == key {
            Ok(())
        } else {
            Err(Error::api(
                ErrorCode::Conflict,
                "the tenant already has another pseudonym key",
            ))
        }
    }

    async fn unwrap_stored(&self, tenant: TenantId, stored: &StoredKey) -> Result<PseudonymKey> {
        if stored.key_version != KEY_VERSION {
            return Err(key_error("only key version 1 exists"));
        }
        self.wrapper.unwrap(tenant, &stored.wrapped).await
    }
}

// ----------------------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    //! One conformance suite for both stores ([`MemoryTenantKeyStore`], [`PgTenantKeyStore`]),
    //! plus the Postgres cases (row-level security, the tenant filter, no plain key in any
    //! table, a restart with the same KEK file), the log check and the OpenBao wrapper.

    use super::*;
    use crate::privacy::{HmacPseudonymizer, Pseudonymizer, SqlValue};
    use sqlx::postgres::PgPoolOptions;
    use std::path::PathBuf;
    use uuid::Uuid;

    const VALUE: SqlValue<'static> = SqlValue::Text("alice@example.com");

    fn pseudonym(key: &PseudonymKey) -> String {
        HmacPseudonymizer::new(key.clone())
            .pseudonym(&VALUE)
            .id()
            .to_string()
    }

    fn kek(byte: u8) -> Arc<LocalKekWrapper> {
        Arc::new(LocalKekWrapper::from_bytes(&[byte; KEK_LEN]))
    }

    /// True when `haystack` holds `needle` anywhere.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Every form in which a key could show in a text: hex (both cases), base64, the `Debug` of
    /// the byte array.
    fn key_forms(key: &[u8]) -> Vec<String> {
        vec![
            hex::encode(key),
            hex::encode_upper(key),
            B64.encode(key),
            format!("{key:?}"),
            // The first 8 bytes alone, in case a text holds a part of the key.
            hex::encode(&key[..8]),
        ]
    }

    struct TempFile(PathBuf);

    impl TempFile {
        fn new() -> Self {
            TempFile(std::env::temp_dir().join(format!("ruum-test-kek-{}", Uuid::now_v7())))
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    // ------------------------------------------------------------------------- harness

    #[async_trait]
    trait Harness: Send + Sync {
        /// A new tenant (a `tenants` row on Postgres).
        async fn tenant(&self) -> TenantId;
        fn store(&self) -> Arc<dyn TenantKeyStore>;
        /// The store of another cell into which `tenant` moves: it holds no key of the tenant.
        async fn empty_cell(&self, tenant: TenantId) -> Arc<dyn TenantKeyStore>;
    }

    struct Memory {
        store: Arc<MemoryTenantKeyStore>,
    }

    #[async_trait]
    impl Harness for Memory {
        async fn tenant(&self) -> TenantId {
            TenantId::new()
        }
        fn store(&self) -> Arc<dyn TenantKeyStore> {
            self.store.clone()
        }
        async fn empty_cell(&self, _tenant: TenantId) -> Arc<dyn TenantKeyStore> {
            Arc::new(MemoryTenantKeyStore::new())
        }
    }

    struct Pg {
        pool: PgPool,
        system: PgPool,
        store: Arc<PgTenantKeyStore>,
    }

    /// A pool whose connections set `ruum.system = on`, to seed and inspect rows across
    /// row-level security. The store under test uses the plain pool.
    async fn system_pool(pool: &PgPool) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("select set_config('ruum.system', 'on', false)")
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap()
    }

    impl Pg {
        async fn new(pool: PgPool) -> Self {
            Pg {
                system: system_pool(&pool).await,
                store: Arc::new(PgTenantKeyStore::new(pool.clone())),
                pool,
            }
        }
    }

    #[async_trait]
    impl Harness for Pg {
        async fn tenant(&self) -> TenantId {
            let id = TenantId::new();
            let mut tx = begin_tenant(&self.pool, id).await.unwrap();
            sqlx::query(
                "insert into tenants (id, name, kind, created_kind, created_by) \
                 values ($1, 'Test', 'team', 'user', $2)",
            )
            .bind(id.0)
            .bind(Uuid::now_v7())
            .execute(&mut *tx)
            .await
            .unwrap();
            tx.commit().await.unwrap();
            id
        }
        fn store(&self) -> Arc<dyn TenantKeyStore> {
            self.store.clone()
        }
        async fn empty_cell(&self, tenant: TenantId) -> Arc<dyn TenantKeyStore> {
            // One test database stands for both cells: the tenant left this cell, so its row is
            // gone here before the import.
            sqlx::query("delete from tenant_keys where tenant_id = $1")
                .bind(tenant.0)
                .execute(&self.system)
                .await
                .unwrap();
            Arc::new(PgTenantKeyStore::new(self.pool.clone()))
        }
    }

    // --------------------------------------------------------------------------- cases

    async fn first_use_makes_one_wrapped_key(h: &dyn Harness) {
        let t = h.tenant().await;
        let wrapper = kek(1);
        let keys = TenantKeys::new(h.store(), wrapper.clone());
        assert!(h.store().get(t).await.unwrap().is_none());
        let k1 = keys.tenant_key(t).await.unwrap();
        let k2 = keys.tenant_key(t).await.unwrap();
        assert_eq!(k1, k2);
        assert_eq!(pseudonym(&k1), pseudonym(&k2));

        let stored = h.store().get(t).await.unwrap().expect("the key is stored");
        // Never rotated: version 1.
        assert_eq!(stored.key_version, 1);
        assert_eq!(stored.tenant, t);
        assert_eq!(stored.wrapped.kek_id, wrapper.kek_id());
        // nonce || ciphertext || tag, inside the check of 0004 (44 to 128).
        assert_eq!(stored.wrapped.bytes.len(), 60);
        assert!(!contains(&stored.wrapped.bytes, k1.as_bytes()));
        assert!(!contains(&stored.wrapped.bytes, &k1.as_bytes()[..8]));
        // A new TenantKeys on the same store and KEK reads the same key.
        let again = TenantKeys::new(h.store(), kek(1));
        assert_eq!(again.tenant_key(t).await.unwrap(), k1);
    }

    async fn racing_first_uses_keep_one_key(h: &dyn Harness) {
        let t = h.tenant().await;
        let wrapper = kek(2);
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let keys = TenantKeys::new(h.store(), wrapper.clone());
                tokio::spawn(async move { keys.tenant_key(t).await.unwrap() })
            })
            .collect();
        let mut got = Vec::new();
        for task in tasks {
            got.push(task.await.unwrap());
        }
        let stored = h.store().get(t).await.unwrap().unwrap();
        let winner = wrapper.unwrap(t, &stored.wrapped).await.unwrap();
        for k in &got {
            assert_eq!(k, &winner, "every first use returns the stored key");
        }
    }

    async fn database_keys_follow_key_node_id(h: &dyn Harness) {
        let t = h.tenant().await;
        let keys = TenantKeys::new(h.store(), kek(3));
        let tenant_key = keys.tenant_key(t).await.unwrap();
        let live = keys.database_key(t, None).await.unwrap();
        assert_eq!(live, tenant_key);
        let node = NodeId::new();
        let branch = keys.database_key(t, Some(node)).await.unwrap();
        assert_eq!(branch, tenant_key.derive_for_database(node));
        assert_ne!(pseudonym(&branch), pseudonym(&live));
        let other = keys.database_key(t, Some(NodeId::new())).await.unwrap();
        assert_ne!(pseudonym(&other), pseudonym(&branch));
        // The handle does not print.
        assert_eq!(format!("{branch:?}"), "PseudonymKey(<hidden>)");
    }

    async fn tenants_are_isolated(h: &dyn Harness) {
        let a = h.tenant().await;
        let b = h.tenant().await;
        let wrapper = kek(4);
        let keys = TenantKeys::new(h.store(), wrapper.clone());
        let ka = keys.tenant_key(a).await.unwrap();
        // B has no key although A has one.
        assert!(h.store().get(b).await.unwrap().is_none());
        assert!(keys.export_for(b, kek(9).as_ref()).await.unwrap().is_none());
        let kb = keys.tenant_key(b).await.unwrap();
        assert_ne!(ka, kb);
        assert_ne!(pseudonym(&ka), pseudonym(&kb));
        let ra = h.store().get(a).await.unwrap().unwrap();
        let rb = h.store().get(b).await.unwrap().unwrap();
        assert_eq!((ra.tenant, rb.tenant), (a, b));
        assert_ne!(ra.wrapped, rb.wrapped);
        // A wrapped key is bound to its tenant: A's row does not unwrap as B's key.
        assert!(wrapper.unwrap(b, &ra.wrapped).await.is_err());
        // insert_if_missing of B never returns or replaces A's row.
        let again = h.store().insert_if_missing(b, &ra.wrapped).await.unwrap();
        assert_eq!(again.wrapped, rb.wrapped);
        assert_eq!(h.store().get(a).await.unwrap().unwrap().wrapped, ra.wrapped);
    }

    async fn export_and_import_keep_the_pseudonyms(h: &dyn Harness) {
        let t = h.tenant().await;
        let source_kek = kek(5);
        let target_kek = kek(6);
        let source = TenantKeys::new(h.store(), source_kek.clone());
        let key = source.tenant_key(t).await.unwrap();
        let node = NodeId::new();
        let live = pseudonym(&key);
        let draft = pseudonym(&source.database_key(t, Some(node)).await.unwrap());

        let exported = source
            .export_for(t, target_kek.as_ref())
            .await
            .unwrap()
            .expect("the tenant has a key");
        assert_eq!(exported.tenant, t);
        assert_eq!(exported.key_version, 1);
        assert_eq!(exported.wrapped.kek_id, target_kek.kek_id());
        assert!(!contains(&exported.wrapped.bytes, key.as_bytes()));
        // Re-wrapped: the source KEK cannot read it.
        assert!(source_kek.unwrap(t, &exported.wrapped).await.is_err());

        let wrong = source
            .export_for(t, kek(7).as_ref())
            .await
            .unwrap()
            .unwrap();
        let target_store = h.empty_cell(t).await;
        let target = TenantKeys::new(target_store.clone(), target_kek.clone());
        // An export for another KEK does not import here.
        assert!(target.import(&wrong).await.is_err());
        assert!(target_store.get(t).await.unwrap().is_none());

        target.import(&exported).await.unwrap();
        let stored = target_store.get(t).await.unwrap().unwrap();
        assert_eq!(stored.wrapped.kek_id, target_kek.kek_id());
        assert_eq!(stored.key_version, 1);
        assert_eq!(pseudonym(&target.tenant_key(t).await.unwrap()), live);
        assert_eq!(
            pseudonym(&target.database_key(t, Some(node)).await.unwrap()),
            draft
        );
        // The same import again is a no-op; another key for the tenant is refused.
        target.import(&exported).await.unwrap();
        let other = ExportedKey {
            wrapped: target_kek.wrap(t, &random_key()).await.unwrap(),
            ..exported.clone()
        };
        let err = target.import(&other).await.unwrap_err();
        assert_eq!(err.to_api().code, ErrorCode::Conflict);
        // Key rotation is not specified: no other version imports.
        let v2 = ExportedKey {
            key_version: 2,
            ..exported.clone()
        };
        assert_eq!(
            target.import(&v2).await.unwrap_err().to_api().code,
            ErrorCode::ValidationFailed
        );
        assert_eq!(target.tenant_key(t).await.unwrap(), key);
    }

    async fn run_all(h: &dyn Harness) {
        first_use_makes_one_wrapped_key(h).await;
        racing_first_uses_keep_one_key(h).await;
        database_keys_follow_key_node_id(h).await;
        tenants_are_isolated(h).await;
        export_and_import_keep_the_pseudonyms(h).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn memory_tenant_keys_conform() {
        run_all(&Memory {
            store: Arc::new(MemoryTenantKeyStore::new()),
        })
        .await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_tenant_keys_conform(pool: PgPool) {
        run_all(&Pg::new(pool).await).await;
    }

    // ------------------------------------------------------------------ Postgres cases

    /// Row-level security of `tenant_keys` (the policy of 0004, FORCEd; the test role is no
    /// superuser): a transaction of tenant B sees and writes only B's rows.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_row_level_security_holds_the_tenant(pool: PgPool) {
        let h = Pg::new(pool.clone()).await;
        let (a, b) = (h.tenant().await, h.tenant().await);
        let keys = TenantKeys::new(h.store(), kek(10));
        keys.tenant_key(a).await.unwrap();
        keys.tenant_key(b).await.unwrap();

        let superuser: bool =
            sqlx::query_scalar("select rolsuper from pg_roles where rolname = current_user")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            !superuser,
            "the test role must not be a superuser (it would bypass RLS)"
        );

        // No tenant set: no row.
        let n: i64 = sqlx::query_scalar("select count(*) from tenant_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
        // A query without a tenant filter in B's transaction sees only B.
        let mut tx = begin_tenant(&pool, b).await.unwrap();
        let seen: Vec<Uuid> = sqlx::query_scalar("select tenant_id from tenant_keys")
            .fetch_all(&mut *tx)
            .await
            .unwrap();
        assert_eq!(seen, vec![b.0]);
        // B cannot write a row of A.
        let denied = sqlx::query(
            "insert into tenant_keys (tenant_id, purpose, key_version, wrapped_key, kek_id) \
             values ($1, 'pseudonym', 1, $2, 'local:x') on conflict do nothing",
        )
        .bind(a.0)
        .bind(vec![0u8; 60])
        .execute(&mut *tx)
        .await;
        assert!(denied.is_err(), "the policy's with check refuses A's row");
        tx.rollback().await.unwrap();
        // The table refuses another version: the key is never rotated.
        let v2 = sqlx::query(
            "insert into tenant_keys (tenant_id, purpose, key_version, wrapped_key, kek_id) \
             values ($1, 'pseudonym', 2, $2, 'local:x')",
        )
        .bind(a.0)
        .bind(vec![0u8; 60])
        .execute(&h.system)
        .await;
        assert!(v2.is_err());
    }

    /// The tenant filter of the store's queries alone: with FORCE off, the owner (the test
    /// role) bypasses the policy, so only `tenant_id = $1` keeps B from A's row. The proposed
    /// mutation of `mutation_check.sh` drops it and must fail this test.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: PgPool) {
        let h = Pg::new(pool.clone()).await;
        let (a, b) = (h.tenant().await, h.tenant().await);
        sqlx::query("alter table tenant_keys no force row level security")
            .execute(&pool)
            .await
            .unwrap();
        let wrapper = kek(11);
        let keys = TenantKeys::new(h.store(), wrapper.clone());
        let ka = keys.tenant_key(a).await.unwrap();
        assert!(h.store().get(b).await.unwrap().is_none());
        let kb = keys.tenant_key(b).await.unwrap();
        assert_ne!(ka, kb);
        assert_eq!(h.store().get(b).await.unwrap().unwrap().tenant, b);
        assert_eq!(
            wrapper
                .unwrap(b, &h.store().get(b).await.unwrap().unwrap().wrapped)
                .await
                .unwrap(),
            kb
        );
    }

    /// No platform table holds a plain key: after first uses, exports and imports, and after the
    /// key paths of card D-1 (the engine key of a new database, handed to an engine that makes
    /// the database), no row of any table of the database holds the key in any form of
    /// [`key_forms`] (hex in both cases, base64, the `Debug` of the bytes, a part).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_no_plain_key_in_any_table(pool: PgPool) {
        let h = Pg::new(pool.clone()).await;
        let t = h.tenant().await;
        let keys = TenantKeys::new(h.store(), kek(12));
        let key = keys.tenant_key(t).await.unwrap();
        let node = NodeId::new();
        let derived = keys.database_key(t, Some(node)).await.unwrap();
        // D-1: createDatabase hands the engine key (key_node_id NULL) to the engine.
        {
            use ruum_dbengine::DatabaseEngine;
            let engine = ruum_dbengine::memory::MemoryEngine::new();
            let handle = keys.engine_key(t, None).await.unwrap();
            assert_eq!(format!("{handle:?}"), "PseudonymKey(<hidden>)");
            engine
                .create_database(
                    &ruum_dbengine::NewDatabase {
                        tenant_id: t.0,
                        node_id: node.0,
                        engine_version: "3".into(),
                        extensions: Vec::new(),
                    },
                    &handle,
                )
                .await
                .unwrap();
            let _ = keys.engine_key(t, Some(node)).await.unwrap();
        }
        let exported = keys.export_for(t, kek(13).as_ref()).await.unwrap().unwrap();
        let target = TenantKeys::new(h.empty_cell(t).await, kek(13));
        target.import(&exported).await.unwrap();
        assert_eq!(target.tenant_key(t).await.unwrap(), key);

        let tables: Vec<String> = sqlx::query_scalar(
            "select quote_ident(table_schema) || '.' || quote_ident(table_name) \
             from information_schema.tables \
             where table_type = 'BASE TABLE' \
               and table_schema not in ('pg_catalog', 'information_schema')",
        )
        .fetch_all(&h.system)
        .await
        .unwrap();
        assert!(tables.iter().any(|t| t.ends_with(".tenant_keys")));
        let rows: i64 = sqlx::query_scalar("select count(*) from tenant_keys")
            .fetch_one(&h.system)
            .await
            .unwrap();
        assert_eq!(rows, 1);
        for k in [&key, &derived] {
            for form in key_forms(k.as_bytes()) {
                for table in &tables {
                    let n: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                        "select count(*) from {table} as r \
                         where strpos(r::text, $1) > 0 or strpos(lower(r::text), lower($1)) > 0"
                    )))
                    .bind(&form)
                    .fetch_one(&h.system)
                    .await
                    .unwrap();
                    assert_eq!(n, 0, "{table} holds a plain key");
                }
            }
        }
    }

    /// A restart of the laptop stack: the wrapped key stays in Postgres and the KEK in its
    /// file. With every in-memory object dropped, a new wrapper on the same file gives the same
    /// pseudonyms.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_restart_with_the_kek_file_keeps_the_pseudonyms(pool: PgPool) {
        let file = TempFile::new();
        LocalKekWrapper::create_file(&file.0).unwrap();
        assert_eq!(std::fs::metadata(&file.0).unwrap().len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file.0).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Never overwritten.
        let before = std::fs::read(&file.0).unwrap();
        assert!(LocalKekWrapper::create_file(&file.0).is_err());
        assert_eq!(std::fs::read(&file.0).unwrap(), before);

        let (t, node, live, draft) = {
            let h = Pg::new(pool.clone()).await;
            let t = h.tenant().await;
            let keys =
                TenantKeys::new(h.store(), Arc::new(LocalKekWrapper::open(&file.0).unwrap()));
            let node = NodeId::new();
            let live = pseudonym(&keys.database_key(t, None).await.unwrap());
            let draft = pseudonym(&keys.database_key(t, Some(node)).await.unwrap());
            (t, node, live, draft)
        };
        // A new connection pool, a new store and a new wrapper on the same file.
        let fresh = PgPoolOptions::new()
            .max_connections(2)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        let keys = TenantKeys::new(
            Arc::new(PgTenantKeyStore::new(fresh.clone())),
            Arc::new(LocalKekWrapper::open(&file.0).unwrap()),
        );
        assert_eq!(pseudonym(&keys.database_key(t, None).await.unwrap()), live);
        assert_eq!(
            pseudonym(&keys.database_key(t, Some(node)).await.unwrap()),
            draft
        );
        let stored = PgTenantKeyStore::new(fresh).get(t).await.unwrap().unwrap();
        assert_eq!(stored.key_version, 1);
        // Another KEK file cannot read the key (a lost KEK file loses the key).
        let other = TempFile::new();
        LocalKekWrapper::create_file(&other.0).unwrap();
        let wrong = TenantKeys::new(
            Arc::new(PgTenantKeyStore::new(pool)),
            Arc::new(LocalKekWrapper::open(&other.0).unwrap()),
        );
        assert!(wrong.tenant_key(t).await.is_err());
    }

    // ----------------------------------------------------------------- local wrapper

    #[tokio::test]
    async fn local_wrapper_wraps_with_a_new_nonce_and_checks_tenant_and_kek() {
        let w = kek(20);
        let t = TenantId::new();
        let key = random_key();
        let w1 = w.wrap(t, &key).await.unwrap();
        let w2 = w.wrap(t, &key).await.unwrap();
        assert_eq!(w1.bytes.len(), 60);
        assert_ne!(w1.bytes[..12], w2.bytes[..12], "a random nonce per wrap");
        assert_ne!(w1.bytes, w2.bytes);
        assert_eq!(w.unwrap(t, &w1).await.unwrap(), key);
        assert_eq!(w.unwrap(t, &w2).await.unwrap(), key);
        // Another tenant (the AAD), a changed byte, a cut key, another KEK: no unwrap.
        assert!(w.unwrap(TenantId::new(), &w1).await.is_err());
        let mut bad = w1.clone();
        bad.bytes[30] ^= 1;
        assert!(w.unwrap(t, &bad).await.is_err());
        let mut short = w1.clone();
        short.bytes.pop();
        assert!(w.unwrap(t, &short).await.is_err());
        let err = kek(21).unwrap(t, &w1).await.unwrap_err().to_string();
        assert!(err.contains("wrapped by local:"), "{err}");
        // The kek_id names the KEK without showing it.
        assert!(w.kek_id().starts_with("local:"));
        assert_eq!(w.kek_id().len(), "local:".len() + 16);
        assert!(!w.kek_id().contains(&hex::encode([20u8; 8])));
        assert_ne!(w.kek_id(), kek(21).kek_id());
        assert!(!format!("{w:?}").contains(&hex::encode([20u8; 4])));
    }

    #[test]
    fn kek_file_must_hold_32_bytes() {
        let file = TempFile::new();
        std::fs::write(&file.0, b"0123456789abcdef0123456789abcdefXX").unwrap();
        let err = LocalKekWrapper::open(&file.0).unwrap_err().to_string();
        assert!(err.contains("exactly 32 bytes"), "{err}");
        assert!(!err.contains("0123456789abcdef"), "{err}");
        assert!(LocalKekWrapper::open(file.0.with_extension("missing")).is_err());
        std::fs::write(&file.0, [7u8; 32]).unwrap();
        assert_eq!(
            LocalKekWrapper::open(&file.0).unwrap().kek_id(),
            LocalKekWrapper::from_bytes(&[7u8; 32]).kek_id()
        );
    }

    #[test]
    fn key_comparison_is_by_value() {
        let a = PseudonymKey::from_bytes([1; 32]);
        let mut last = [1; 32];
        last[31] = 2;
        assert_eq!(a, PseudonymKey::from_bytes([1; 32]));
        assert_ne!(a, PseudonymKey::from_bytes(last));
        assert_ne!(a, PseudonymKey::from_bytes([0; 32]));
    }

    // ---------------------------------------------------------------------- the log

    mod capture {
        use std::fmt::Write as _;
        use std::sync::{Arc, Mutex};
        use tracing::field::{Field, Visit};
        use tracing::span::{Attributes, Id, Record};
        use tracing::subscriber::Interest;
        use tracing::{Event, Metadata, Subscriber};

        /// A subscriber that writes every span field and event (all levels) into one text.
        #[derive(Clone, Default)]
        pub struct Capture(pub Arc<Mutex<String>>);

        struct Visitor<'a>(&'a mut String);

        impl Visit for Visitor<'_> {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                let _ = write!(self.0, "{}={:?} ", field.name(), value);
            }
            fn record_str(&mut self, field: &Field, value: &str) {
                let _ = write!(self.0, "{}={} ", field.name(), value);
            }
        }

        impl Subscriber for Capture {
            fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
                Interest::always()
            }
            fn enabled(&self, _: &Metadata<'_>) -> bool {
                true
            }
            fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
                Some(tracing::level_filters::LevelFilter::TRACE)
            }
            fn new_span(&self, span: &Attributes<'_>) -> Id {
                let mut out = self.0.lock().unwrap();
                let _ = write!(out, "\n[span {}] ", span.metadata().name());
                span.record(&mut Visitor(&mut out));
                Id::from_u64(1)
            }
            fn record(&self, _: &Id, values: &Record<'_>) {
                values.record(&mut Visitor(&mut self.0.lock().unwrap()));
            }
            fn record_follows_from(&self, _: &Id, _: &Id) {}
            fn event(&self, event: &Event<'_>) {
                let mut out = self.0.lock().unwrap();
                let _ = write!(
                    out,
                    "\n[{} {}] ",
                    event.metadata().level(),
                    event.metadata().target()
                );
                event.record(&mut Visitor(&mut out));
            }
            fn enter(&self, _: &Id) {}
            fn exit(&self, _: &Id) {}
        }
    }

    /// Runs wrap, unwrap, first use, the database key, export and import with every event
    /// captured, and checks that no key shows in the log.
    async fn exercise_and_check_the_log(store: Arc<dyn TenantKeyStore>, tenant: TenantId) {
        let capture = capture::Capture::default();
        let guard = tracing::subscriber::set_default(capture.clone());
        // Callsites that other tests hit first, without a subscriber, may have cached "never".
        tracing::callsite::rebuild_interest_cache();
        let w = kek(30);
        let keys = TenantKeys::new(store.clone(), w.clone());
        let key = keys.tenant_key(tenant).await.unwrap();
        let again = keys.tenant_key(tenant).await.unwrap();
        let node = NodeId::new();
        let derived = keys.database_key(tenant, Some(node)).await.unwrap();
        let loose = random_key();
        let wrapped = w.wrap(tenant, &loose).await.unwrap();
        assert_eq!(w.unwrap(tenant, &wrapped).await.unwrap(), loose);
        let exported = keys
            .export_for(tenant, kek(31).as_ref())
            .await
            .unwrap()
            .unwrap();
        let other = TenantKeys::new(Arc::new(MemoryTenantKeyStore::new()), kek(31));
        other.import(&exported).await.unwrap();
        // Errors are logged by callers: their text holds no key either.
        let errors = [
            kek(32)
                .unwrap(tenant, &wrapped)
                .await
                .unwrap_err()
                .to_string(),
            w.unwrap(TenantId::new(), &wrapped)
                .await
                .unwrap_err()
                .to_string(),
        ];
        tracing::debug!(
            ?keys,
            ?w,
            ?key,
            ?wrapped,
            ?exported,
            ?errors,
            "debug of every object"
        );
        drop(guard);
        assert_eq!(key, again);

        let log = capture.0.lock().unwrap().clone();
        assert!(
            log.contains("made the pseudonym key of the tenant"),
            "the capture works: {log}"
        );
        for k in [&key, &derived, &loose] {
            for form in key_forms(k.as_bytes()) {
                assert!(!log.contains(&form), "the log holds a key ({form}): {log}");
            }
        }
        for form in key_forms(&[30u8; 32]) {
            assert!(!log.contains(&form), "the log holds the KEK: {log}");
        }
    }

    #[tokio::test]
    async fn no_key_in_the_log_memory() {
        exercise_and_check_the_log(Arc::new(MemoryTenantKeyStore::new()), TenantId::new()).await;
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn no_key_in_the_log_postgres(pool: PgPool) {
        let h = Pg::new(pool).await;
        let t = h.tenant().await;
        exercise_and_check_the_log(h.store(), t).await;
    }

    // ----------------------------------------------------------------------- OpenBao

    /// The OpenBao of the dev stack: `RUUM_OPENBAO_ADDR` and `RUUM_OPENBAO_TOKEN`, or `BAO_ADDR`
    /// and `BAO_TOKEN` of `scripts/dev.sh env`. `None` (the test is skipped) without them: CI
    /// has no OpenBao service yet.
    fn openbao_env() -> Option<(String, String)> {
        let get = |a: &str, b: &str| {
            std::env::var(a)
                .ok()
                .or_else(|| std::env::var(b).ok())
                .filter(|v| !v.is_empty())
        };
        Some((
            get(OPENBAO_ADDR_ENV, "BAO_ADDR")?,
            get(OPENBAO_TOKEN_ENV, "BAO_TOKEN")?,
        ))
    }

    /// Enables the transit engine and the key, as `scripts/dev.sh up` does (idempotent).
    async fn ensure_transit(addr: &str, token: &str) {
        let http = reqwest::Client::new();
        let mounts = http
            .get(format!("{addr}/v1/sys/mounts/{OPENBAO_TRANSIT_MOUNT}"))
            .header("X-Vault-Token", token)
            .send()
            .await
            .unwrap();
        if !mounts.status().is_success() {
            let res = http
                .post(format!("{addr}/v1/sys/mounts/{OPENBAO_TRANSIT_MOUNT}"))
                .header("X-Vault-Token", token)
                .json(&serde_json::json!({ "type": "transit" }))
                .send()
                .await
                .unwrap();
            // 400: a parallel run enabled it first.
            assert!(
                res.status().is_success() || res.status() == 400,
                "{}",
                res.status()
            );
        }
        let res = http
            .post(format!(
                "{addr}/v1/{OPENBAO_TRANSIT_MOUNT}/keys/{OPENBAO_TRANSIT_KEY}"
            ))
            .header("X-Vault-Token", token)
            .json(&serde_json::json!({ "type": "aes256-gcm96" }))
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success(), "{}", res.status());
    }

    #[tokio::test]
    async fn openbao_transit_wrapper_round_trip() {
        let Some((addr, token)) = openbao_env() else {
            eprintln!("skipped: no OpenBao ({OPENBAO_ADDR_ENV} / BAO_ADDR not set)");
            return;
        };
        ensure_transit(&addr, &token).await;
        let w = Arc::new(
            OpenBaoTransitWrapper::new(
                &addr,
                token.clone(),
                OPENBAO_TRANSIT_MOUNT,
                OPENBAO_TRANSIT_KEY,
            )
            .unwrap(),
        );
        assert!(!format!("{w:?}").contains(&token));
        let t = TenantId::new();
        let key = random_key();
        let wrapped = w.wrap(t, &key).await.unwrap();
        assert!(
            wrapped
                .kek_id
                .starts_with("openbao-transit:transit/ruum-tenant-keys:v"),
            "{}",
            wrapped.kek_id
        );
        assert!(
            (44..=128).contains(&wrapped.bytes.len()),
            "{}",
            wrapped.bytes.len()
        );
        assert!(!contains(&wrapped.bytes, key.as_bytes()));
        assert!(!contains(
            &wrapped.bytes,
            B64.encode(key.as_bytes()).as_bytes()
        ));
        assert_eq!(w.unwrap(t, &wrapped).await.unwrap(), key);
        // Bound to the tenant (associated data); a local wrapped key is refused by its kek_id.
        assert!(w.unwrap(TenantId::new(), &wrapped).await.is_err());
        let local = kek(40).wrap(t, &key).await.unwrap();
        assert!(w.unwrap(t, &local).await.is_err());

        // The whole round trip: first use under OpenBao, export to a local KEK and back.
        let store = Arc::new(MemoryTenantKeyStore::new());
        let keys = TenantKeys::new(store.clone(), w.clone());
        let tk = keys.tenant_key(t).await.unwrap();
        assert_eq!(keys.tenant_key(t).await.unwrap(), tk);
        let stored = store.get(t).await.unwrap().unwrap();
        assert!(stored.wrapped.kek_id.starts_with(w.kek_id()));
        let exported = keys.export_for(t, kek(41).as_ref()).await.unwrap().unwrap();
        let moved = TenantKeys::new(Arc::new(MemoryTenantKeyStore::new()), kek(41));
        moved.import(&exported).await.unwrap();
        assert_eq!(
            pseudonym(&moved.tenant_key(t).await.unwrap()),
            pseudonym(&tk)
        );
        let back = moved.export_for(t, w.as_ref()).await.unwrap().unwrap();
        let home = TenantKeys::new(Arc::new(MemoryTenantKeyStore::new()), w.clone());
        home.import(&back).await.unwrap();
        assert_eq!(home.tenant_key(t).await.unwrap(), tk);
    }

    #[test]
    fn wrapper_from_env_needs_a_kek() {
        // Only reads: the variables of this process decide. With neither set it fails with a
        // hint; the dev stack sets RUUM_LOCAL_KEK_FILE.
        if std::env::var(OPENBAO_ADDR_ENV).is_err() && std::env::var(LOCAL_KEK_FILE_ENV).is_err() {
            let err = match wrapper_from_env() {
                Ok(_) => panic!("a wrapper without a KEK"),
                Err(e) => e.to_string(),
            };
            assert!(err.contains(LOCAL_KEK_FILE_ENV), "{err}");
        }
    }
}
