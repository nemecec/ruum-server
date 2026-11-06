//! The SpiceDB engine, through the SpiceDB HTTP API (the gRPC gateway; `serve --http-enabled`).
//!
//! On the laptop stack the HTTP API listens on `http://127.0.0.1:8543` and the gRPC API on
//! `127.0.0.1:50051`; both take the preshared key (`SPICEDB_PRESHARED_KEY` of
//! `scripts/dev.sh env`). The HTTP API needs no gRPC or protobuf crates.
//!
//! Consistency (decision 0002-4): every write returns a ZedToken. A check, bulk check or read
//! asks for at least the freshness of, in this order: the explicit token of the call; the floor
//! of the request ([`super::request_floor`]), which starts at the tenant's newest applied token
//! from the outbox ([`super::outbox::PgFreshness`], read once per request by
//! `identity::auth::context_for`) and rises with every write of the request; else the newest
//! token that this process wrote (workers and `seed-dev` run outside a request). So a write or
//! revocation applied by any server is seen by the next request on every server.

use super::schema::CONTRACT_SCHEMA;
use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;

/// The largest batch of one bulk check.
const BULK_CHUNK: usize = 500;

/// How to reach SpiceDB.
#[derive(Clone, Debug)]
pub struct SpiceDbConfig {
    /// The base URL of the HTTP API, for example `http://127.0.0.1:8543`.
    pub http_url: String,
    /// The preshared key.
    pub preshared_key: String,
}

impl SpiceDbConfig {
    /// From the environment: `RUUM_SPICEDB_HTTP_URL` (default `http://127.0.0.1:8543`) and
    /// `RUUM_SPICEDB_PRESHARED_KEY` or `SPICEDB_PRESHARED_KEY`. `None` without a key.
    pub fn from_env() -> Option<Self> {
        let var = |n: &str| std::env::var(n).ok().filter(|v| !v.is_empty());
        let preshared_key =
            var("RUUM_SPICEDB_PRESHARED_KEY").or_else(|| var("SPICEDB_PRESHARED_KEY"))?;
        Some(Self {
            http_url: var("RUUM_SPICEDB_HTTP_URL")
                .unwrap_or_else(|| "http://127.0.0.1:8543".into()),
            preshared_key,
        })
    }
}

/// The SpiceDB engine.
#[derive(Debug)]
pub struct SpiceDbAuthz {
    http: reqwest::Client,
    base: String,
    key: String,
    latest: Mutex<Option<String>>,
}

fn engine_error(what: &str, detail: impl std::fmt::Display) -> Error {
    Error::Internal(format!("spicedb {what}: {detail}"))
}

fn object_json(o: &ObjectRef) -> Value {
    json!({ "objectType": o.object_type, "objectId": o.object_id })
}

fn subject_json(s: &SubjectRef) -> Value {
    let mut v = json!({ "object": object_json(&s.object) });
    if let Some(r) = &s.relation {
        v["optionalRelation"] = json!(r);
    }
    v
}

fn relationship_json(r: &Relationship) -> Value {
    json!({
        "resource": object_json(&r.object),
        "relation": r.relation,
        "subject": subject_json(&r.subject),
    })
}

fn object_from(v: &Value) -> Option<ObjectRef> {
    Some(ObjectRef::new(
        v.get("objectType")?.as_str()?,
        v.get("objectId")?.as_str()?,
    ))
}

fn relationship_from(v: &Value) -> Option<Relationship> {
    let subject = v.get("subject")?;
    let relation = subject
        .get("optionalRelation")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
        .map(str::to_string);
    Some(Relationship {
        object: object_from(v.get("resource")?)?,
        relation: v.get("relation")?.as_str()?.to_string(),
        subject: SubjectRef {
            object: object_from(subject.get("object")?)?,
            relation,
        },
    })
}

fn has_permission(v: &Value) -> Result<bool> {
    match v.get("permissionship").and_then(Value::as_str) {
        Some("PERMISSIONSHIP_HAS_PERMISSION") => Ok(true),
        Some("PERMISSIONSHIP_NO_PERMISSION") => Ok(false),
        // Caveats are not used in the Ruum schema.
        other => Err(engine_error(
            "check",
            format!("unexpected answer {other:?}"),
        )),
    }
}

impl SpiceDbAuthz {
    /// A client. It does not talk to SpiceDB yet; call [`SpiceDbAuthz::write_schema`] at start.
    pub fn new(config: SpiceDbConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| engine_error("client", e))?;
        Ok(Self {
            http,
            base: config.http_url.trim_end_matches('/').to_string(),
            key: config.preshared_key,
            latest: Mutex::new(None),
        })
    }

    /// A client that has written the contract schema (`contracts/authz/schema.zed`). The server
    /// calls this at start.
    pub async fn connect(config: SpiceDbConfig) -> Result<Self> {
        let s = Self::new(config)?;
        s.write_schema(CONTRACT_SCHEMA).await?;
        Ok(s)
    }

    /// Writes a schema. Writing the same schema again is a no-op for SpiceDB.
    pub async fn write_schema(&self, schema: &str) -> Result<()> {
        let v = self
            .post("/v1/schema/write", json!({ "schema": schema }))
            .await?;
        if let Some(t) = v
            .get("writtenAt")
            .and_then(|w| w.get("token"))
            .and_then(Value::as_str)
        {
            self.saw(t);
        }
        Ok(())
    }

    /// The schema that SpiceDB holds now, if any.
    pub async fn read_schema(&self) -> Result<Option<String>> {
        let res = self
            .http
            .post(format!("{}/v1/schema/read", self.base))
            .bearer_auth(&self.key)
            .json(&json!({}))
            .send()
            .await
            .map_err(|e| engine_error("schema read", e))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let v = Self::json_of(res, "schema read").await?;
        Ok(v.get("schemaText")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    fn saw(&self, token: &str) {
        // ZedTokens are opaque; the newest write is the one that was made last here.
        *self.latest.lock().expect("token lock") = Some(token.to_string());
        // The request that wrote reads its own write.
        raise_request_floor(&ConsistencyToken(token.to_string()));
    }

    fn consistency(&self, at_least: Option<&ConsistencyToken>) -> Value {
        let token = at_least
            .map(|t| t.0.clone())
            .or_else(|| request_floor().map(|t| t.0))
            .or_else(|| self.latest.lock().expect("token lock").clone());
        match token {
            Some(t) => json!({ "atLeastAsFresh": { "token": t } }),
            None => json!({ "minimizeLatency": true }),
        }
    }

    async fn json_of(res: reqwest::Response, what: &str) -> Result<Value> {
        let status = res.status();
        let text = res.text().await.map_err(|e| engine_error(what, e))?;
        if !status.is_success() {
            return Err(engine_error(what, format!("{status}: {text}")));
        }
        serde_json::from_str(&text).map_err(|e| engine_error(what, e))
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let res = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| engine_error(path, e))?;
        Self::json_of(res, path).await
    }
}

#[async_trait]
impl Authz for SpiceDbAuthz {
    async fn check(
        &self,
        subject: &SubjectRef,
        permission: &str,
        object: &ObjectRef,
        at_least: Option<&ConsistencyToken>,
    ) -> Result<bool> {
        let v = self
            .post(
                "/v1/permissions/check",
                json!({
                    "consistency": self.consistency(at_least),
                    "resource": object_json(object),
                    "permission": permission,
                    "subject": subject_json(subject),
                }),
            )
            .await?;
        has_permission(&v)
    }

    async fn check_bulk(
        &self,
        subject: &SubjectRef,
        permission: &str,
        objects: &[ObjectRef],
    ) -> Result<Vec<bool>> {
        let mut out = Vec::with_capacity(objects.len());
        for chunk in objects.chunks(BULK_CHUNK) {
            let items: Vec<Value> = chunk
                .iter()
                .map(|o| {
                    json!({
                        "resource": object_json(o),
                        "permission": permission,
                        "subject": subject_json(subject),
                    })
                })
                .collect();
            let v = self
                .post(
                    "/v1/permissions/checkbulk",
                    json!({ "consistency": self.consistency(None), "items": items }),
                )
                .await?;
            let pairs = v
                .get("pairs")
                .and_then(Value::as_array)
                .ok_or_else(|| engine_error("checkbulk", "no pairs"))?;
            if pairs.len() != chunk.len() {
                return Err(engine_error("checkbulk", "wrong number of answers"));
            }
            for p in pairs {
                match p.get("item") {
                    Some(item) => out.push(has_permission(item)?),
                    None => return Err(engine_error("checkbulk", p)),
                }
            }
        }
        Ok(out)
    }

    async fn write(&self, updates: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
        let ups: Vec<Value> = updates
            .iter()
            .map(|u| match u {
                RelationshipUpdate::Touch(r) => {
                    json!({ "operation": "OPERATION_TOUCH", "relationship": relationship_json(r) })
                }
                RelationshipUpdate::Delete(r) => {
                    json!({ "operation": "OPERATION_DELETE", "relationship": relationship_json(r) })
                }
            })
            .collect();
        let v = self
            .post("/v1/relationships/write", json!({ "updates": ups }))
            .await?;
        let token = v
            .get("writtenAt")
            .and_then(|w| w.get("token"))
            .and_then(Value::as_str)
            .ok_or_else(|| engine_error("write", "no token"))?
            .to_string();
        self.saw(&token);
        Ok(ConsistencyToken(token))
    }

    async fn read(&self, object: &ObjectRef) -> Result<Vec<Relationship>> {
        let res = self
            .http
            .post(format!("{}/v1/relationships/read", self.base))
            .bearer_auth(&self.key)
            .json(&json!({
                "consistency": self.consistency(None),
                "relationshipFilter": {
                    "resourceType": object.object_type,
                    "optionalResourceId": object.object_id,
                },
            }))
            .send()
            .await
            .map_err(|e| engine_error("read", e))?;
        let status = res.status();
        let text = res.text().await.map_err(|e| engine_error("read", e))?;
        if !status.is_success() {
            return Err(engine_error("read", format!("{status}: {text}")));
        }
        // The gateway streams one JSON object per line: `{"result": {...}}` or `{"error": ...}`.
        let mut out = Vec::new();
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).map_err(|e| engine_error("read", e))?;
            if let Some(err) = v.get("error") {
                return Err(engine_error("read", err));
            }
            let r = v
                .get("result")
                .and_then(|r| r.get("relationship"))
                .and_then(relationship_from)
                .ok_or_else(|| engine_error("read", line))?;
            out.push(r);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The conformance suite against the SpiceDB of the laptop stack. Skipped (with a note) when
    /// `SPICEDB_PRESHARED_KEY` is not set (`eval "$(scripts/dev.sh env)"`).
    #[tokio::test]
    async fn spicedb_authz_conforms() {
        let Some(config) = SpiceDbConfig::from_env() else {
            eprintln!("skipped: SPICEDB_PRESHARED_KEY is not set");
            return;
        };
        let authz = SpiceDbAuthz::connect(config)
            .await
            .expect("connect to SpiceDB");
        let schema = authz.read_schema().await.unwrap().unwrap();
        assert!(schema.contains("editors_cannot_share"));
        let prefix = format!("t{}", uuid::Uuid::now_v7().simple());
        super::super::conformance::run(&authz, &prefix).await;
    }

    #[test]
    fn relationships_map_to_json_and_back() {
        let r: Relationship = "node:r#manager@tenant:t#administer".parse().unwrap();
        assert_eq!(relationship_from(&relationship_json(&r)), Some(r));
        let r: Relationship = "node:s#editors_share_off@user:*".parse().unwrap();
        assert_eq!(relationship_from(&relationship_json(&r)), Some(r));
    }
}
