//! The tokens of the runtime API (decision 0006-10): the checks of workload tokens, of the
//! gateway's user tokens (`Ruum-User`) and of delegated tokens, and the signer of delegated
//! tokens.
//!
//! Every check verifies the EdDSA signature with the key that the header's `kid` names in its
//! own key set (a token without `kid`, or with a `kid` of another set, is refused), then `typ`,
//! `aud`, `iss`, `exp` (no grace), `iat` and `nbf` (at most [`SKEW_SECONDS`] in the future),
//! the longest life of its kind, and the required claims. A refusal names the claim, never a
//! value. Credentials never mix: each kind has its own `typ` and key set, so a token of one kind
//! never passes the check of another (a workload token as a user token, a user token as a
//! workload token, an API token of the REST API as either).

use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use ruum_core::apps::workload_tokens::{
    DELEGATED_TYP, Ed25519Key, INTERNAL_AUDIENCE, WORKLOAD_TOKEN_LIFETIME, WORKLOAD_TYP,
};
use ruum_core::identity::app_sign_in::AppEnv;
use ruum_core::{NodeId, TenantId};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

/// How far in the future `iat` and `nbf` may be (clock skew), in seconds.
pub const SKEW_SECONDS: i64 = 60;
/// The longest life of a delegated token (`exp - iat`), in seconds (the contract: 5 minutes).
pub const DELEGATED_SECONDS: i64 = 300;
/// The longest life of a user token (`exp - iat`), in seconds (the contract: 60).
pub const USER_TOKEN_SECONDS: i64 = 60;
/// `typ` of a user token.
pub const USER_TYP: &str = "app_user";
/// The longest token the runtime API reads.
pub const MAX_TOKEN_BYTES: usize = 8192;

/// Why a token is refused (claim names only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenError(pub String);

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(what: impl Into<String>) -> TokenError {
    TokenError(what.into())
}

fn now() -> i64 {
    ruum_core::now().unix_timestamp()
}

/// A public key set: EdDSA keys by `kid`.
#[derive(Clone)]
pub struct KeySet {
    keys: Vec<(String, DecodingKey)>,
}

impl std::fmt::Debug for KeySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kids: Vec<&str> = self.keys.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("KeySet").field("kids", &kids).finish()
    }
}

impl KeySet {
    /// The keys of a JWKS value. Keys without a `kid` or that are not Ed25519 are left out.
    pub fn from_jwks(v: &Value) -> Result<Self, String> {
        let set: JwkSet =
            serde_json::from_value(v.clone()).map_err(|_| "not a JSON Web Key Set".to_string())?;
        let mut keys = Vec::new();
        for k in &set.keys {
            let Some(kid) = k.common.key_id.clone() else {
                continue;
            };
            if let Ok(d) = DecodingKey::from_jwk(k) {
                keys.push((kid, d));
            }
        }
        if keys.is_empty() {
            return Err("the key set has no usable key".into());
        }
        Ok(Self { keys })
    }

    /// The keys of a JWKS file.
    pub fn from_file(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
        let v: Value =
            serde_json::from_str(&text).map_err(|_| format!("{} is not JSON", path.display()))?;
        Self::from_jwks(&v)
    }

    /// The claims of `token` when its EdDSA signature verifies with the key its `kid` names.
    /// No claim is checked here.
    pub fn verify(&self, token: &str) -> Result<Value, TokenError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(bad("the token is too long"));
        }
        let header = jsonwebtoken::decode_header(token).map_err(|_| bad("not a JWT"))?;
        if header.alg != Algorithm::EdDSA {
            return Err(bad("alg is not EdDSA"));
        }
        let kid = header.kid.ok_or_else(|| bad("the token has no kid"))?;
        let key = self
            .keys
            .iter()
            .find(|(k, _)| *k == kid)
            .map(|(_, d)| d)
            .ok_or_else(|| bad("no key of the key set has the token's kid"))?;
        let mut v = Validation::new(Algorithm::EdDSA);
        v.required_spec_claims.clear();
        v.validate_exp = false;
        v.validate_nbf = false;
        v.validate_aud = false;
        jsonwebtoken::decode::<Value>(token, key, &v)
            .map(|d| d.claims)
            .map_err(|_| bad("the signature does not verify"))
    }
}

/// The `typ` claim of a token, without any check (to pick the checker; never trusted).
pub fn peek_typ(token: &str) -> Option<String> {
    use base64::Engine as _;
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    v.get("typ").and_then(Value::as_str).map(str::to_string)
}

fn string<'a>(c: &'a Value, k: &str) -> Result<&'a str, TokenError> {
    c.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| bad(format!("{k} is missing")))
}

fn uuid(c: &Value, k: &str) -> Result<Uuid, TokenError> {
    Uuid::parse_str(string(c, k)?).map_err(|_| bad(format!("{k} is not a UUID")))
}

fn env(c: &Value) -> Result<AppEnv, TokenError> {
    AppEnv::parse(string(c, "env")?).ok_or_else(|| bad("env is not live or preview"))
}

/// The common claim checks: `typ`, `iss`, numeric `iat` and `exp`, `exp` after now, `iat` and
/// `nbf` not beyond the skew, `exp - iat` between 1 and `max_life`, a `jti`.
fn times_and_kind(
    c: &Value,
    typ: &str,
    issuer: &str,
    max_life: i64,
) -> Result<(i64, i64), TokenError> {
    if c.get("typ").and_then(Value::as_str) != Some(typ) {
        return Err(bad(format!("typ is not {typ}")));
    }
    if c.get("iss").and_then(Value::as_str) != Some(issuer) {
        return Err(bad("iss is not this instance"));
    }
    let (Some(iat), Some(exp)) = (
        c.get("iat").and_then(Value::as_i64),
        c.get("exp").and_then(Value::as_i64),
    ) else {
        return Err(bad("iat and exp must both be numbers"));
    };
    let t = now();
    if exp <= t {
        return Err(bad("exp has passed"));
    }
    if iat > t + SKEW_SECONDS {
        return Err(bad("iat is in the future"));
    }
    match c.get("nbf") {
        None => {}
        Some(n) => match n.as_i64() {
            Some(n) if n <= t + SKEW_SECONDS => {}
            Some(_) => return Err(bad("nbf is in the future")),
            None => return Err(bad("nbf is not a number")),
        },
    }
    if exp <= iat || exp - iat > max_life {
        return Err(bad(format!(
            "exp - iat must be between 1 and {max_life} seconds"
        )));
    }
    string(c, "jti")?;
    Ok((iat, exp))
}

/// A verified workload token: the calling principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workload {
    pub tenant: TenantId,
    pub app: NodeId,
    /// The app node ID (live) or the deployment ID (preview).
    pub principal: Uuid,
    pub environment: AppEnv,
}

/// Checks a workload token (see the module docs). A live token's `sub` must be its `app`.
pub fn verify_workload(token: &str, keys: &KeySet, issuer: &str) -> Result<Workload, TokenError> {
    let c = keys.verify(token)?;
    if c.get("aud").and_then(Value::as_str) != Some(INTERNAL_AUDIENCE) {
        return Err(bad("aud is not ruum.internal"));
    }
    times_and_kind(
        &c,
        WORKLOAD_TYP,
        issuer,
        WORKLOAD_TOKEN_LIFETIME.as_secs() as i64,
    )?;
    let w = Workload {
        tenant: TenantId(uuid(&c, "tid")?),
        app: NodeId(uuid(&c, "app")?),
        principal: uuid(&c, "sub")?,
        environment: env(&c)?,
    };
    if w.environment == AppEnv::Live && w.principal != w.app.0 {
        return Err(bad("the live principal is not its app"));
    }
    if w.environment == AppEnv::Preview && w.principal == w.app.0 {
        return Err(bad("a preview principal is its deployment"));
    }
    Ok(w)
}

/// A verified user token (`contracts/apps/user-token.schema.json`), as the exchange needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserToken {
    pub aud: String,
    pub env: String,
    pub sub: String,
    /// The tenant slug.
    pub tenant: String,
    pub actor: String,
    pub on_behalf_of: Option<String>,
    pub jti: String,
    pub exp: i64,
}

/// Checks a user token against the gateway's key set: signature, `typ` `app_user`, `iss`,
/// `exp`, at most [`USER_TOKEN_SECONDS`] long, and the claims the exchange reads. `aud`, `env`
/// and `tenant` are compared with the caller by the exchange.
pub fn verify_user(token: &str, keys: &KeySet, issuer: &str) -> Result<UserToken, TokenError> {
    let c = keys.verify(token)?;
    let (_, exp) = times_and_kind(&c, USER_TYP, issuer, USER_TOKEN_SECONDS)?;
    Ok(UserToken {
        aud: string(&c, "aud")?.to_string(),
        env: string(&c, "env")?.to_string(),
        sub: string(&c, "sub")?.to_string(),
        tenant: string(&c, "tenant")?.to_string(),
        actor: string(&c, "actor")?.to_string(),
        on_behalf_of: c
            .get("on_behalf_of")
            .and_then(Value::as_str)
            .map(str::to_string),
        jti: string(&c, "jti")?.to_string(),
        exp,
    })
}

/// The audience a user token must have for this caller: `app:<app>` live,
/// `app:<app>/preview/<deployment>` for a preview.
pub fn expected_audience(app: NodeId, environment: AppEnv, principal: Uuid) -> String {
    match environment {
        AppEnv::Live => format!("app:{}", app.0),
        AppEnv::Preview => format!("app:{}/preview/{principal}", app.0),
    }
}

/// A verified delegated token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delegated {
    pub caller: Workload,
    pub bindings: Vec<String>,
    /// The sealed user (`on_behalf_of`).
    pub user: Uuid,
    /// The sealed agent (`agent`).
    pub agent: Option<Uuid>,
}

/// The claims of a delegated token (`contracts/apps/delegated-token.schema.json`; `app`, the
/// app node of a preview principal, is an extra claim).
#[derive(Clone, Debug, Serialize)]
pub struct DelegatedClaims {
    pub iss: String,
    pub aud: String,
    pub typ: String,
    pub sub: Uuid,
    pub tid: TenantId,
    pub app: NodeId,
    pub env: String,
    pub act: Value,
    pub on_behalf_of: Uuid,
    pub agent: Option<Uuid>,
    pub chain: Vec<String>,
    pub bindings: Vec<String>,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
}

/// Signs and checks delegated tokens with the runtime API's own key
/// (`RUUM_RUNTIME_DELEGATED_KEY_FILE`).
pub struct DelegatedTokens {
    issuer: String,
    key: Ed25519Key,
    keys: KeySet,
}

impl std::fmt::Debug for DelegatedTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegatedTokens")
            .field("kid", &self.key.kid)
            .finish_non_exhaustive()
    }
}

/// A binding name of the schema (`^[a-z][a-z0-9_]{0,31}$`).
pub fn valid_binding_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

impl DelegatedTokens {
    pub fn new(issuer: &str, key: Ed25519Key) -> Result<Self, String> {
        let keys = KeySet::from_jwks(&key.jwks())?;
        Ok(Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            key,
            keys,
        })
    }

    /// The public key set (what the T-6 world gives the cases as `delegated_jwks`).
    pub fn jwks(&self) -> Value {
        self.key.jwks()
    }

    /// A delegated token for `caller` acting for the sealed `user` through `bindings`, valid
    /// for [`DELEGATED_SECONDS`].
    pub fn sign(
        &self,
        caller: &Workload,
        bindings: Vec<String>,
        user: Uuid,
        agent: Option<Uuid>,
    ) -> Result<String, String> {
        let t = now();
        let claims = DelegatedClaims {
            iss: self.issuer.clone(),
            aud: INTERNAL_AUDIENCE.into(),
            typ: DELEGATED_TYP.into(),
            sub: caller.principal,
            tid: caller.tenant,
            app: caller.app,
            env: caller.environment.as_str().into(),
            act: json!({ "kind": "app", "id": caller.principal }),
            on_behalf_of: user,
            agent,
            chain: Vec::new(),
            bindings,
            iat: t,
            exp: t + DELEGATED_SECONDS,
            jti: Uuid::now_v7().to_string(),
        };
        self.key.sign(&claims).map_err(|e| e.to_string())
    }

    /// Checks a delegated token of this runtime API (see the module docs).
    pub fn verify(&self, token: &str) -> Result<Delegated, TokenError> {
        let c = self.keys.verify(token)?;
        if c.get("aud").and_then(Value::as_str) != Some(INTERNAL_AUDIENCE) {
            return Err(bad("aud is not ruum.internal"));
        }
        times_and_kind(&c, DELEGATED_TYP, &self.issuer, DELEGATED_SECONDS)?;
        let caller = Workload {
            tenant: TenantId(uuid(&c, "tid")?),
            app: NodeId(uuid(&c, "app")?),
            principal: uuid(&c, "sub")?,
            environment: env(&c)?,
        };
        let act = c.get("act").ok_or_else(|| bad("act is missing"))?;
        if act.get("kind").and_then(Value::as_str) != Some("app")
            || act.get("id").and_then(Value::as_str) != Some(&caller.principal.to_string())
        {
            return Err(bad("act is not the app"));
        }
        let bindings: Vec<String> = c
            .get("bindings")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("bindings is missing"))?
            .iter()
            .map(|b| b.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or_else(|| bad("bindings holds a value that is not a name"))?;
        if bindings.is_empty() || !bindings.iter().all(|b| valid_binding_name(b)) {
            return Err(bad("bindings is empty or not names"));
        }
        let agent = match c.get("agent") {
            None | Some(Value::Null) => None,
            Some(_) => Some(uuid(&c, "agent")?),
        };
        Ok(Delegated {
            caller,
            bindings,
            user: uuid(&c, "on_behalf_of")?,
            agent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::apps::workload_tokens::{WorkloadClaims, WorkloadTokenIssuer};
    use ruum_core::runtime::{Environment, WorkloadIdentity};

    const ISS: &str = "https://ruum.localhost";

    fn identity(env: Environment) -> WorkloadIdentity {
        WorkloadIdentity {
            tenant: TenantId(Uuid::now_v7()),
            app: NodeId(Uuid::now_v7()),
            deployment: Uuid::now_v7(),
            environment: env,
        }
    }

    fn keys(i: &WorkloadTokenIssuer) -> KeySet {
        KeySet::from_jwks(&i.jwks()).unwrap()
    }

    #[test]
    fn a_workload_token_verifies_and_names_its_principal() {
        let i = WorkloadTokenIssuer::generate(ISS);
        let id = identity(Environment::Preview);
        let w = verify_workload(i.issue(&id).unwrap().expose(), &keys(&i), ISS).unwrap();
        assert_eq!(w.principal, id.deployment);
        assert_eq!(w.app, id.app);
        assert_eq!(w.environment, AppEnv::Preview);
    }

    /// Each bad claim is refused: typ (a delegated token as a workload token), aud, iss, exp,
    /// iat and nbf beyond the skew, a too long life, a missing claim, a live principal that is
    /// not its app, a foreign key, a missing kid.
    #[test]
    fn bad_workload_tokens_are_refused() {
        let i = WorkloadTokenIssuer::generate(ISS);
        let ks = keys(&i);
        let id = identity(Environment::Live);
        let good = i.claims(&id);
        assert!(verify_workload(i.sign(&good).unwrap().expose(), &ks, ISS).is_ok());
        let t = now();
        type Spoil = fn(&mut WorkloadClaims, i64);
        let spoils: [(&str, Spoil); 9] = [
            ("typ app_delegated", |c, _| c.typ = DELEGATED_TYP.into()),
            ("typ app_user", |c, _| c.typ = USER_TYP.into()),
            ("aud of an app", |c, _| c.aud = format!("app:{}", c.app.0)),
            ("another issuer", |c, _| {
                c.iss = "https://evil.example".into()
            }),
            ("expired", |c, t| {
                c.iat = t - 1000;
                c.nbf = t - 1000;
                c.exp = t - 1;
            }),
            ("iat in the future", |c, t| {
                c.iat = t + 120;
                c.exp = t + 600;
            }),
            ("nbf in the future", |c, t| c.nbf = t + 120),
            ("life over 15 minutes", |c, t| {
                c.iat = t;
                c.exp = t + 901;
            }),
            ("a live principal that is not its app", |c, _| {
                c.sub = Uuid::now_v7()
            }),
        ];
        for (what, f) in spoils {
            let mut c = good.clone();
            f(&mut c, t);
            assert!(
                verify_workload(i.sign(&c).unwrap().expose(), &ks, ISS).is_err(),
                "{what} passed"
            );
        }
        let other = WorkloadTokenIssuer::generate(ISS);
        assert!(verify_workload(other.sign(&good).unwrap().expose(), &ks, ISS).is_err());
        // A missing claim (env).
        let mut v = serde_json::to_value(&good).unwrap();
        v.as_object_mut().unwrap().remove("env");
        let k = Ed25519Key::generate();
        let ks2 = KeySet::from_jwks(&k.jwks()).unwrap();
        assert!(verify_workload(&k.sign(&v).unwrap(), &ks2, ISS).is_err());
        // No kid.
        let mut h = jsonwebtoken::Header::new(Algorithm::EdDSA);
        h.kid = None;
        let no_kid = jsonwebtoken::encode(&h, &good, &k.encoding).unwrap();
        assert!(verify_workload(&no_kid, &ks2, ISS).is_err());
    }

    #[test]
    fn a_delegated_token_round_trips_and_is_no_workload_token() {
        let d = DelegatedTokens::new(ISS, Ed25519Key::generate()).unwrap();
        let w = Workload {
            tenant: TenantId(Uuid::now_v7()),
            app: NodeId(Uuid::now_v7()),
            principal: Uuid::now_v7(),
            environment: AppEnv::Preview,
        };
        let user = Uuid::now_v7();
        let t = d.sign(&w, vec!["doc".into()], user, None).unwrap();
        let v = d.verify(&t).unwrap();
        assert_eq!(
            (v.caller.clone(), v.user, v.bindings.clone()),
            (w, user, vec!["doc".to_string()])
        );
        // Its own key set never verifies it as a workload token (typ).
        let ks = KeySet::from_jwks(&d.jwks()).unwrap();
        assert!(verify_workload(&t, &ks, ISS).is_err());
        // A workload token never passes as a delegated token.
        let i = WorkloadTokenIssuer::generate(ISS);
        let wt = i.issue(&identity(Environment::Live)).unwrap();
        assert!(d.verify(wt.expose()).is_err());
    }

    #[test]
    fn audiences_follow_the_environment() {
        let app = NodeId(Uuid::now_v7());
        let dep = Uuid::now_v7();
        assert_eq!(
            expected_audience(app, AppEnv::Live, app.0),
            format!("app:{}", app.0)
        );
        assert_eq!(
            expected_audience(app, AppEnv::Preview, dep),
            format!("app:{}/preview/{dep}", app.0)
        );
    }
}
