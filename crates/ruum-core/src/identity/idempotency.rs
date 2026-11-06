//! Idempotency keys of writes (table `idempotency_keys`; the `Idempotency-Key` header of
//! `contracts/openapi.yaml`).
//!
//! A retry with the same key and the same request within 24 hours gets the first response. The
//! same key with another request fails with `409 idempotency_key_reused`. While the first request
//! runs, a retry fails with `409 conflict`. A first request that ends with a server error leaves
//! no row, so the retry runs again.
//!
//! Keys belong to the actor and a tenant: the tenant the credentials are bound to, else the
//! tenant of the request (the `Ruum-Tenant` header or the `/v1/tenants/{id}` path) when the
//! caller is a member or guest there, else the user's home tenant. The HTTP layer
//! (`ruum_api::auth::idempotency`) calls this store.
//!
//! A response that holds a secret shown once (an agent API key, an app password, a refresh
//! token, an OAuth client secret, an invitation token), or that says `Cache-Control: no-store`,
//! is never kept: the stores keep [`secret_withheld`] in its place, so a retry gets
//! `409 conflict` and the secret is never in the database (decision 0001-7: only SHA-256).

use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::TenantId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use time::OffsetDateTime;
use uuid::Uuid;

/// How long a key is kept.
pub const KEY_HOURS: i64 = 24;

/// A stored response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The visible prefixes of the secrets that Ruum shows once.
pub const SECRET_PREFIXES: &[&str] = &[
    crate::agents::API_KEY_PREFIX,
    crate::identity::APP_PASSWORD_PREFIX,
    crate::agents::oauth::REFRESH_PREFIX,
    crate::agents::oauth::CLIENT_PREFIX,
    crate::tenants::INVITATION_PREFIX,
];

/// The length of the random part of a secret (`secret::random_secret`: 43 base64url characters).
const SECRET_BODY_LEN: usize = 43;

/// Whether `body` holds a whole secret: one of [`SECRET_PREFIXES`] followed by at least 43
/// base64url characters. A display prefix (`ruum_agent_AbCd`) is not a secret.
pub fn body_holds_secret(body: &[u8]) -> bool {
    SECRET_PREFIXES.iter().any(|p| {
        let p = p.as_bytes();
        body.windows(p.len()).enumerate().any(|(i, w)| {
            w == p
                && body[i + p.len()..]
                    .iter()
                    .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                    .count()
                    >= SECRET_BODY_LEN
        })
    })
}

/// Whether a response must not be kept: it holds a secret, or says `Cache-Control: no-store`.
pub fn must_not_store(response: &StoredResponse) -> bool {
    body_holds_secret(&response.body)
        || response.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("cache-control") && v.to_ascii_lowercase().contains("no-store")
        })
}

/// What is kept in place of a response that must not be kept: `409 conflict`, which tells the
/// client that the first request succeeded but its answer is not shown again.
pub fn secret_withheld(status: u16) -> StoredResponse {
    let error = ApiError::new(
        ErrorCode::Conflict,
        format!(
            "the first request with this Idempotency-Key succeeded (HTTP {status}); its answer \
             held a secret that is shown only once, so it is not kept"
        ),
    )
    .with_hint(
        "list the resource to find what the first request made; if its secret is lost, revoke \
         it and make a new one with a new key",
    );
    StoredResponse {
        status: 409,
        headers: vec![("content-type".into(), "application/json".into())],
        body: serde_json::to_vec(&serde_json::json!({ "error": error })).unwrap_or_default(),
    }
}

/// The response as it may be kept: `response`, or [`secret_withheld`] when it must not be kept.
/// Every [`IdempotencyStore::finish`] stores this.
pub fn storable(response: &StoredResponse) -> StoredResponse {
    if must_not_store(response) {
        secret_withheld(response.status)
    } else {
        response.clone()
    }
}

/// What [`IdempotencyStore::begin`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Begin {
    /// The key is new; the request runs and its response is stored with `finish`.
    Fresh,
    /// The same request ran before: send this response again.
    Replay(StoredResponse),
}

/// Who and where a key belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyRef {
    pub tenant: TenantId,
    pub actor: Uuid,
    pub key: String,
}

/// The hash of a request: method, path and query, tenant header and body.
pub fn request_hash(method: &str, uri: &str, tenant_header: Option<&str>, body: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(method.as_bytes());
    h.update([0]);
    h.update(uri.as_bytes());
    h.update([0]);
    h.update(tenant_header.unwrap_or_default().as_bytes());
    h.update([0]);
    h.update(body);
    h.finalize().into()
}

/// Checks the key: 8 to 255 printable characters.
pub fn check_key(key: &str) -> Result<()> {
    if (8..=255).contains(&key.len()) && key.bytes().all(|b| (0x21..0x7f).contains(&b)) {
        Ok(())
    } else {
        Err(Error::Api(
            ApiError::new(
                ErrorCode::BadRequest,
                "the Idempotency-Key must have 8 to 255 printable characters",
            )
            .with_hint("use a UUID"),
        ))
    }
}

pub fn key_reused() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::IdempotencyKeyReused,
            "this Idempotency-Key was used for another request",
        )
        .with_hint("use a new key for a new request"),
    )
}

pub fn in_flight() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::Conflict,
            "a request with this Idempotency-Key is still running",
        )
        .with_hint("retry in a moment"),
    )
}

#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Claims the key for a request, or finds the stored response. Fails with
    /// `idempotency_key_reused` for another request and `conflict` while the first one runs.
    async fn begin(
        &self,
        key: &KeyRef,
        method: &str,
        path: &str,
        request_hash: [u8; 32],
    ) -> Result<Begin>;
    /// Stores the response of the request that claimed the key, as [`storable`] gives it (never
    /// a secret).
    async fn finish(&self, key: &KeyRef, response: &StoredResponse) -> Result<()>;
    /// Gives the key up (the request failed with a server error), so a retry runs again.
    async fn abandon(&self, key: &KeyRef) -> Result<()>;
}

struct Row {
    hash: [u8; 32],
    response: Option<StoredResponse>,
    expires_at: OffsetDateTime,
}

/// The in-memory store.
#[derive(Default)]
pub struct MemoryIdempotencyStore {
    rows: Mutex<HashMap<KeyRef, Row>>,
}

#[async_trait]
impl IdempotencyStore for MemoryIdempotencyStore {
    async fn begin(
        &self,
        key: &KeyRef,
        _method: &str,
        _path: &str,
        request_hash: [u8; 32],
    ) -> Result<Begin> {
        let now = crate::now();
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.retain(|_, r| r.expires_at > now);
        match rows.get(key) {
            Some(r) if r.hash != request_hash => Err(key_reused()),
            Some(Row {
                response: Some(resp),
                ..
            }) => Ok(Begin::Replay(resp.clone())),
            Some(_) => Err(in_flight()),
            None => {
                rows.insert(
                    key.clone(),
                    Row {
                        hash: request_hash,
                        response: None,
                        expires_at: now + time::Duration::hours(KEY_HOURS),
                    },
                );
                Ok(Begin::Fresh)
            }
        }
    }

    async fn finish(&self, key: &KeyRef, response: &StoredResponse) -> Result<()> {
        if let Some(r) = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(key)
        {
            r.response = Some(storable(response));
        }
        Ok(())
    }

    async fn abandon(&self, key: &KeyRef) -> Result<()> {
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resp(body: &str, headers: Vec<(&str, &str)>) -> StoredResponse {
        StoredResponse {
            status: 201,
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn secrets_are_found_but_display_prefixes_are_not() {
        let key = crate::identity::secret::random_secret(crate::agents::API_KEY_PREFIX);
        let pw = crate::identity::secret::random_secret(crate::identity::APP_PASSWORD_PREFIX);
        assert!(body_holds_secret(
            format!(r#"{{"secret":"{key}"}}"#).as_bytes()
        ));
        assert!(body_holds_secret(
            format!(r#"{{"secret":"{pw}"}}"#).as_bytes()
        ));
        assert!(!body_holds_secret(
            br#"{"prefix":"ruum_agent_AbCd","name":"bot"}"#
        ));
        assert!(!body_holds_secret(b"{}"));
    }

    #[test]
    fn a_secret_or_no_store_is_withheld() {
        let key = crate::identity::secret::random_secret(crate::agents::API_KEY_PREFIX);
        let kept = storable(&resp(&format!(r#"{{"secret":"{key}"}}"#), vec![]));
        assert_eq!(kept.status, 409);
        assert!(!String::from_utf8_lossy(&kept.body).contains("ruum_agent_"));
        let kept = storable(&resp("{}", vec![("cache-control", "no-store")]));
        assert_eq!(kept.status, 409);
        let plain = resp(r#"{"id":"x"}"#, vec![("content-type", "application/json")]);
        assert_eq!(storable(&plain), plain);
    }

    #[tokio::test]
    async fn the_memory_store_never_keeps_a_secret() {
        let store = MemoryIdempotencyStore::default();
        let k = KeyRef {
            tenant: TenantId::new(),
            actor: Uuid::now_v7(),
            key: "key-00000001".into(),
        };
        assert_eq!(
            store.begin(&k, "POST", "/x", [1; 32]).await.unwrap(),
            Begin::Fresh
        );
        let key = crate::identity::secret::random_secret(crate::agents::API_KEY_PREFIX);
        store
            .finish(&k, &resp(&format!(r#"{{"secret":"{key}"}}"#), vec![]))
            .await
            .unwrap();
        let Begin::Replay(r) = store.begin(&k, "POST", "/x", [1; 32]).await.unwrap() else {
            panic!("expected a replay");
        };
        assert_eq!(r.status, 409);
        assert!(!String::from_utf8_lossy(&r.body).contains(&key));
    }
}
