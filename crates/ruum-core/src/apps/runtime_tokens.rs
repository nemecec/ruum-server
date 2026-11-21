//! The checks of the tokens at `ruum.internal` (card R-3, decision 0006-10): workload tokens,
//! the gateway's user tokens (`Ruum-User`) and delegated tokens. Both the runtime API
//! (`ruum-runtime-api`, its `token` module re-exports this one) and the core's internal routes
//! for the runtime API use them: the core checks every token again and takes the caller only
//! from a token that passes (review of R-3, required change 1).
//!
//! Every check verifies the EdDSA signature with the key that the header's `kid` names in its
//! own key set (a token without `kid`, or with a `kid` of another set, is refused), then `typ`,
//! `aud`, `iss`, `exp` (no grace), `iat` and `nbf` (at most [`SKEW_SECONDS`] in the future),
//! the longest life of its kind, and the required claims. A refusal names the claim, never a
//! value. Credentials never mix: each kind has its own `typ`, so a token of one kind never
//! passes the check of another (a workload token as a delegated token, a user token as a
//! workload token, an API token of the REST API as either).
//!
//! Delegated tokens are signed by the core with the workload-token key
//! (`delegated-token.schema.json`: "EdDSA, the workload-token keys";
//! [`crate::apps::workload_tokens::WorkloadTokenIssuer::sign_delegated`]), so the runtime API
//! holds no signing key at all and checks both kinds with the one public key set.
//!
//! Both kinds carry `dep`, the deployment the token was made for (an extra claim; for a preview
//! it is `sub`). The core serves a live token only while `dep` is the app's live deployment, so
//! an instance of a replaced live deployment stops working when the new one is published.

use crate::apps::workload_tokens::{DELEGATED_TYP, INTERNAL_AUDIENCE, WORKLOAD_TYP};
use crate::identity::app_sign_in::AppEnv;
use crate::ids::{NodeId, TenantId};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
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
/// The longest token that is read at all.
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
    crate::now().unix_timestamp()
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

/// The `typ` claim of a token, without any check (to pick the checker; never trusted). A token
/// over [`MAX_TOKEN_BYTES`] has none.
pub fn peek_typ(token: &str) -> Option<String> {
    use base64::Engine as _;
    if token.len() > MAX_TOKEN_BYTES {
        return None;
    }
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
    /// The deployment the token was made for (`dep`; a preview's is its principal).
    pub deployment: Uuid,
    pub environment: AppEnv,
}

/// The principal claims of a workload or delegated token: `tid`, `app`, `sub`, `dep`, `env`.
/// A live principal is its app; a preview principal is its deployment (`dep == sub`).
fn principal_claims(c: &Value) -> Result<Workload, TokenError> {
    let w = Workload {
        tenant: TenantId(uuid(c, "tid")?),
        app: NodeId(uuid(c, "app")?),
        principal: uuid(c, "sub")?,
        deployment: uuid(c, "dep")?,
        environment: env(c)?,
    };
    match w.environment {
        AppEnv::Live if w.principal != w.app.0 => Err(bad("the live principal is not its app")),
        AppEnv::Preview if w.principal == w.app.0 => {
            Err(bad("a preview principal is its deployment"))
        }
        AppEnv::Preview if w.deployment != w.principal => {
            Err(bad("dep of a preview is not its principal"))
        }
        _ => Ok(w),
    }
}

/// Checks a workload token (see the module docs).
pub fn verify_workload(token: &str, keys: &KeySet, issuer: &str) -> Result<Workload, TokenError> {
    let c = keys.verify(token)?;
    if c.get("aud").and_then(Value::as_str) != Some(INTERNAL_AUDIENCE) {
        return Err(bad("aud is not ruum.internal"));
    }
    times_and_kind(
        &c,
        WORKLOAD_TYP,
        issuer,
        crate::apps::workload_tokens::WORKLOAD_TOKEN_LIFETIME.as_secs() as i64,
    )?;
    principal_claims(&c)
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
/// app node of a preview principal, and `dep`, the deployment, are extra claims).
#[derive(Clone, Debug, Serialize)]
pub struct DelegatedClaims {
    pub iss: String,
    pub aud: String,
    pub typ: String,
    pub sub: Uuid,
    pub tid: TenantId,
    pub app: NodeId,
    pub dep: Uuid,
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

impl DelegatedClaims {
    /// The claims of a new delegated token of `caller` acting for the sealed `user` through
    /// `bindings`, valid for [`DELEGATED_SECONDS`] from now.
    pub fn new(
        issuer: &str,
        caller: &Workload,
        bindings: Vec<String>,
        user: Uuid,
        agent: Option<Uuid>,
    ) -> Self {
        let t = now();
        Self {
            iss: issuer.trim_end_matches('/').to_string(),
            aud: INTERNAL_AUDIENCE.into(),
            typ: DELEGATED_TYP.into(),
            sub: caller.principal,
            tid: caller.tenant,
            app: caller.app,
            dep: caller.deployment,
            env: caller.environment.as_str().into(),
            act: json!({ "kind": "app", "id": caller.principal }),
            on_behalf_of: user,
            agent,
            chain: Vec::new(),
            bindings,
            iat: t,
            exp: t + DELEGATED_SECONDS,
            jti: Uuid::now_v7().to_string(),
        }
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

/// Checks a delegated token (see the module docs).
pub fn verify_delegated(token: &str, keys: &KeySet, issuer: &str) -> Result<Delegated, TokenError> {
    let c = keys.verify(token)?;
    if c.get("aud").and_then(Value::as_str) != Some(INTERNAL_AUDIENCE) {
        return Err(bad("aud is not ruum.internal"));
    }
    times_and_kind(&c, DELEGATED_TYP, issuer, DELEGATED_SECONDS)?;
    let caller = principal_claims(&c)?;
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

/// A verified credential at `ruum.internal`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    Workload(Workload),
    Delegated(Delegated),
}

impl Credential {
    /// The calling principal.
    pub fn workload(&self) -> &Workload {
        match self {
            Credential::Workload(w) => w,
            Credential::Delegated(d) => &d.caller,
        }
    }
}

/// Checks a workload token, or (when `delegated`) also a delegated token, picked by its `typ`.
pub fn verify_credential(
    token: &str,
    keys: &KeySet,
    issuer: &str,
    delegated: bool,
) -> Result<Credential, TokenError> {
    match peek_typ(token).as_deref() {
        Some(WORKLOAD_TYP) => verify_workload(token, keys, issuer).map(Credential::Workload),
        Some(DELEGATED_TYP) if delegated => {
            verify_delegated(token, keys, issuer).map(Credential::Delegated)
        }
        _ => Err(bad("not a token of this call")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::workload_tokens::{Ed25519Key, WorkloadClaims, WorkloadTokenIssuer};
    use crate::runtime::{Environment, WorkloadIdentity};

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
        assert_eq!(w.deployment, id.deployment);
        assert_eq!(w.app, id.app);
        assert_eq!(w.environment, AppEnv::Preview);
        let live = identity(Environment::Live);
        let w = verify_workload(i.issue(&live).unwrap().expose(), &keys(&i), ISS).unwrap();
        assert_eq!((w.principal, w.deployment), (live.app.0, live.deployment));
    }

    /// Each bad claim is refused: typ (a delegated token as a workload token), aud, iss, exp,
    /// iat and nbf beyond the skew, a too long life, a missing claim, a live principal that is
    /// not its app, a preview whose dep is not its principal, a foreign key, a missing kid.
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
        // A preview whose dep is another deployment.
        let p = identity(Environment::Preview);
        let mut c = i.claims(&p);
        assert!(verify_workload(i.sign(&c).unwrap().expose(), &ks, ISS).is_ok());
        c.dep = Uuid::now_v7();
        assert!(verify_workload(i.sign(&c).unwrap().expose(), &ks, ISS).is_err());
        let other = WorkloadTokenIssuer::generate(ISS);
        assert!(verify_workload(other.sign(&good).unwrap().expose(), &ks, ISS).is_err());
        // A missing claim (env, dep).
        let k = Ed25519Key::generate();
        let ks2 = KeySet::from_jwks(&k.jwks()).unwrap();
        for claim in ["env", "dep"] {
            let mut v = serde_json::to_value(&good).unwrap();
            v.as_object_mut().unwrap().remove(claim);
            assert!(
                verify_workload(&k.sign(&v).unwrap(), &ks2, ISS).is_err(),
                "no {claim} passed"
            );
        }
        // No kid.
        let mut h = jsonwebtoken::Header::new(Algorithm::EdDSA);
        h.kid = None;
        let no_kid = jsonwebtoken::encode(&h, &good, &k.encoding).unwrap();
        assert!(verify_workload(&no_kid, &ks2, ISS).is_err());
        // Too long to read.
        assert!(peek_typ(&"a".repeat(MAX_TOKEN_BYTES + 1)).is_none());
    }

    #[test]
    fn a_delegated_token_round_trips_and_is_no_workload_token() {
        let i = WorkloadTokenIssuer::generate(ISS);
        let ks = keys(&i);
        let dep = Uuid::now_v7();
        let w = Workload {
            tenant: TenantId(Uuid::now_v7()),
            app: NodeId(Uuid::now_v7()),
            principal: dep,
            deployment: dep,
            environment: AppEnv::Preview,
        };
        let user = Uuid::now_v7();
        let t = i
            .sign_delegated(&w, vec!["doc".into()], user, None)
            .unwrap();
        let v = verify_delegated(t.expose(), &ks, ISS).unwrap();
        assert_eq!(
            (v.caller.clone(), v.user, v.bindings.clone()),
            (w, user, vec!["doc".to_string()])
        );
        // The same key set never verifies it as a workload token (typ), and a workload token
        // never passes as a delegated token.
        assert!(verify_workload(t.expose(), &ks, ISS).is_err());
        let wt = i.issue(&identity(Environment::Live)).unwrap();
        assert!(verify_delegated(wt.expose(), &ks, ISS).is_err());
        // The picker takes a delegated token only where the call allows one.
        assert!(verify_credential(t.expose(), &ks, ISS, false).is_err());
        assert!(matches!(
            verify_credential(t.expose(), &ks, ISS, true),
            Ok(Credential::Delegated(_))
        ));
        assert!(matches!(
            verify_credential(wt.expose(), &ks, ISS, false),
            Ok(Credential::Workload(_))
        ));
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
