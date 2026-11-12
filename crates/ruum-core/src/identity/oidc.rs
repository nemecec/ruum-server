//! The OIDC client for Zitadel (decision 6): authorization code flow with PKCE, run by the
//! server. It reads the provider's discovery document, exchanges the code at the token endpoint
//! and verifies the ID token with the provider's JWKS (signature, issuer, audience, expiry and
//! nonce).
//!
//! On the laptop the client is the public Zitadel app `ruum-web` (PKCE, no secret;
//! `RUUM_OIDC_CLIENT_ID`) with the redirect URI `http://127.0.0.1:8490/v1/auth/callback` or
//! `https://ruum.localhost/v1/auth/callback`. A confidential client sends its secret
//! (`RUUM_OIDC_CLIENT_SECRET`) with HTTP Basic at the token endpoint.
//!
//! The `idp` hint `google` or `microsoft` becomes the reserved Zitadel scope
//! `urn:zitadel:iam:org:idp:id:<id>` when the settings hold an IdP ID for it
//! (`RUUM_OIDC_IDP_GOOGLE`, `RUUM_OIDC_IDP_MICROSOFT`): Zitadel then sends the browser straight to
//! that provider (<https://zitadel.com/docs/apis/openidoauth/scopes>, "Reserved Scopes"). The
//! providers of staging are instance providers in the default login policy
//! (`deploy/staging/ops/zitadel-bootstrap.sh`), so no organization scope is needed. Without an ID,
//! and for `passkey`, Zitadel shows its own choice on its login page.

use super::{LoginStart, OidcClient, OidcSettings, VerifiedLogin, secret, unauthenticated};
use crate::error::{Error, Result};
use async_trait::async_trait;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::Deserialize;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// How long the provider's JWKS is cached.
const JWKS_TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
    #[serde(default)]
    id_token_signing_alg_values_supported: Vec<String>,
}

/// The signature algorithms that an ID token may use: public-key algorithms only (never `none`
/// or HMAC), and of those only the ones the provider lists in
/// `id_token_signing_alg_values_supported` (`RS256` when it lists none, as OIDC Discovery says).
const ACCEPTED_ALGORITHMS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

/// The algorithms accepted for a provider that lists `supported`.
fn allowed_algorithms(supported: &[String]) -> Vec<Algorithm> {
    let listed: Vec<&str> = if supported.is_empty() {
        vec!["RS256"]
    } else {
        supported.iter().map(String::as_str).collect()
    };
    ACCEPTED_ALGORITHMS
        .iter()
        .copied()
        .filter(|a| listed.iter().any(|l| alg_name(*a) == *l))
        .collect()
}

/// The JWA name of an algorithm (`RS256`, ...).
fn alg_name(a: Algorithm) -> String {
    format!("{a:?}")
}

/// Picks the key for a token header: the key with the token's `kid`, or, for a token without a
/// `kid`, the only key of the set. The key's `alg`, when it names one, must be the token's.
fn pick_key<'a>(set: &'a JwkSet, kid: Option<&str>, alg: Algorithm) -> Option<&'a Jwk> {
    let jwk = match kid {
        Some(kid) => set.find(kid)?,
        None if set.keys.len() == 1 => &set.keys[0],
        None => return None,
    };
    match &jwk.common.key_algorithm {
        Some(k) if k.to_string() != alg_name(alg) => None,
        _ => Some(jwk),
    }
}

#[derive(Debug, Deserialize)]
struct TokenAnswer {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdClaims {
    sub: String,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    preferred_username: Option<String>,
}

/// An OIDC client for one provider.
pub struct HttpOidcClient {
    settings: OidcSettings,
    http: reqwest::Client,
    discovery: tokio::sync::OnceCell<Discovery>,
    jwks: RwLock<Option<(JwkSet, Instant)>>,
    /// The URL the provider sends the browser to after sign-out.
    post_logout_redirect_uri: Option<String>,
}

impl HttpOidcClient {
    pub fn new(settings: OidcSettings, post_logout_redirect_uri: Option<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Internal(format!("http client: {e}")))?;
        Ok(Self {
            settings,
            http,
            discovery: tokio::sync::OnceCell::new(),
            jwks: RwLock::new(None),
            post_logout_redirect_uri,
        })
    }

    async fn discovery(&self) -> Result<&Discovery> {
        self.discovery
            .get_or_try_init(|| async {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    self.settings.issuer.trim_end_matches('/')
                );
                let d: Discovery = self
                    .http
                    .get(&url)
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| unavailable(&format!("OIDC discovery: {e}")))?
                    .json()
                    .await
                    .map_err(|e| unavailable(&format!("OIDC discovery: {e}")))?;
                Ok::<_, Error>(d)
            })
            .await
    }

    async fn jwks(&self, refresh: bool) -> Result<JwkSet> {
        if !refresh
            && let Some((set, at)) = self.jwks.read().unwrap_or_else(|e| e.into_inner()).as_ref()
            && at.elapsed() < JWKS_TTL
        {
            return Ok(set.clone());
        }
        let d = self.discovery().await?;
        let set: JwkSet = self
            .http
            .get(&d.jwks_uri)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| unavailable(&format!("OIDC keys: {e}")))?
            .json()
            .await
            .map_err(|e| unavailable(&format!("OIDC keys: {e}")))?;
        *self.jwks.write().unwrap_or_else(|e| e.into_inner()) = Some((set.clone(), Instant::now()));
        Ok(set)
    }

    /// Verifies an ID token and returns its claims.
    async fn verify_id_token(&self, token: &str, nonce: &str) -> Result<IdClaims> {
        let bad = |m: &str| unauthenticated(m, "start the sign-in again");
        let header = decode_header(token).map_err(|_| bad("the ID token is not valid"))?;
        let d = self.discovery().await?;
        // The algorithm of the header is used only when the provider may use it.
        if !allowed_algorithms(&d.id_token_signing_alg_values_supported).contains(&header.alg) {
            return Err(bad(
                "the ID token is signed with an algorithm that is not allowed",
            ));
        }
        let kid = header.kid.as_deref();
        let mut set = self.jwks(false).await?;
        if pick_key(&set, kid, header.alg).is_none() {
            // The provider may have rotated its keys.
            set = self.jwks(true).await?;
        }
        let jwk = pick_key(&set, kid, header.alg)
            .ok_or_else(|| bad("the ID token is signed with an unknown key"))?;
        let key = DecodingKey::from_jwk(jwk).map_err(|_| bad("the provider key is not usable"))?;
        let mut v = Validation::new(header.alg);
        v.set_issuer(&[d.issuer.as_str()]);
        v.set_audience(&[self.settings.client_id.as_str()]);
        v.leeway = 60;
        let claims = decode::<IdClaims>(token, &key, &v)
            .map_err(|_| bad("the ID token is not valid"))?
            .claims;
        if claims.nonce.as_deref() != Some(nonce) {
            return Err(bad("the ID token nonce does not match"));
        }
        Ok(claims)
    }
}

fn unavailable(m: &str) -> Error {
    Error::Api(
        crate::error::ApiError::new(
            crate::error::ErrorCode::Unavailable,
            "the identity provider is not reachable",
        )
        .with_hint(m.to_string()),
    )
}

/// The `scope` of the authorization request: the configured scopes (default
/// `openid email profile`), and the Zitadel IdP scope when `idp_hint` has a configured IdP ID.
/// The ID is checked again here, so a bad setting never reaches the URL.
fn login_scopes(settings: &OidcSettings, idp_hint: Option<&str>) -> String {
    let mut scopes = if settings.scopes.is_empty() {
        "openid email profile".to_string()
    } else {
        settings.scopes.join(" ")
    };
    if let Some(id) = idp_hint
        .and_then(|h| settings.idp_ids.get(h))
        .and_then(|id| super::zitadel_idp_id(id))
    {
        scopes.push_str(" urn:zitadel:iam:org:idp:id:");
        scopes.push_str(id);
    }
    scopes
}

#[async_trait]
impl OidcClient for HttpOidcClient {
    async fn start_login(
        &self,
        return_to: Option<String>,
        idp_hint: Option<String>,
    ) -> Result<LoginStart> {
        let d = self.discovery().await?;
        let state = secret::random_secret("");
        let nonce = secret::random_secret("");
        let pkce_verifier = secret::random_secret("");
        let mut url = url::Url::parse(&d.authorization_endpoint)
            .map_err(|e| Error::Internal(format!("authorization endpoint: {e}")))?;
        let scopes = login_scopes(&self.settings, idp_hint.as_deref());
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.settings.client_id)
            .append_pair("redirect_uri", &self.settings.redirect_uri)
            .append_pair("scope", &scopes)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &secret::pkce_challenge(&pkce_verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(LoginStart {
            authorize_url: url.to_string(),
            state,
            nonce,
            pkce_verifier,
            return_to: return_to.unwrap_or_else(|| "/".into()),
        })
    }

    async fn finish_login(&self, code: &str, start: &LoginStart) -> Result<VerifiedLogin> {
        let d = self.discovery().await?;
        let form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", self.settings.redirect_uri.as_str()),
            ("client_id", self.settings.client_id.as_str()),
            ("code_verifier", start.pkce_verifier.as_str()),
        ];
        let mut req = self.http.post(&d.token_endpoint);
        if let Some(s) = &self.settings.client_secret {
            req = req.basic_auth(&self.settings.client_id, Some(s));
        }
        let res = req
            .form(&form)
            .send()
            .await
            .map_err(|e| unavailable(&format!("OIDC token endpoint: {e}")))?;
        if !res.status().is_success() {
            return Err(unauthenticated(
                "the identity provider refused the sign-in code",
                "start the sign-in again",
            ));
        }
        let answer: TokenAnswer = res
            .json()
            .await
            .map_err(|e| unavailable(&format!("OIDC token endpoint: {e}")))?;
        let id_token = answer.id_token.ok_or_else(|| {
            unauthenticated(
                "the identity provider sent no ID token",
                "start the sign-in again",
            )
        })?;
        let c = self.verify_id_token(&id_token, &start.nonce).await?;
        Ok(VerifiedLogin {
            issuer: d.issuer.clone(),
            subject: c.sub,
            email: c.email,
            email_verified: c.email_verified.unwrap_or(false),
            name: c.name.or(c.preferred_username),
            id_token: Some(id_token),
        })
    }

    async fn logout_url(&self, id_token_hint: Option<String>) -> Result<String> {
        let d = self.discovery().await?;
        let Some(end) = &d.end_session_endpoint else {
            return Ok(self
                .post_logout_redirect_uri
                .clone()
                .unwrap_or_else(|| "/".into()));
        };
        let mut url =
            url::Url::parse(end).map_err(|e| Error::Internal(format!("end session: {e}")))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &self.settings.client_id);
            if let Some(h) = &id_token_hint {
                q.append_pair("id_token_hint", h);
            }
            if let Some(p) = &self.post_logout_redirect_uri {
                q.append_pair("post_logout_redirect_uri", p);
            }
        }
        Ok(url.to_string())
    }
}

#[cfg(test)]
mod algorithm_tests {
    use super::*;

    fn set(keys: serde_json::Value) -> JwkSet {
        serde_json::from_value(serde_json::json!({ "keys": keys })).unwrap()
    }

    fn rsa_key(kid: &str, alg: Option<&str>) -> serde_json::Value {
        let mut k = serde_json::json!({
            "kty": "RSA", "kid": kid, "use": "sig",
            "n": "sXchDaQebHnPiGvyDOAT4saGEUetSyo9MKLOoWFsueri23bOdgWp4Dy1WlUzewbgBHod5pcM9H95GQRV3JDXboIRROSBigeC5yjU1hGzHHyXss8UDprecbAYxknTcQkhslANGRUZmdTOQ5qTRsLAt6BTYuyvVRdhS8exSZEy_c4gs_7svlJJQ4H9_NxsiIoLwAEk7-Q3UXERGYw_75IDrGA84-lA_-Ct4eTlXHBIY2EaV7t7LjJaynVJCpkv4LKjTTAumiGUIuQhrNhZLuF_RJLqHpM2kgWFLU7-VTdL1VbC2tejvcI2BlMkEpk1BzBZI0KQB0GaDWFLN-aEAw3vRw",
            "e": "AQAB"
        });
        if let Some(a) = alg {
            k["alg"] = a.into();
        }
        k
    }

    #[test]
    fn only_listed_public_key_algorithms_are_allowed() {
        assert_eq!(allowed_algorithms(&[]), vec![Algorithm::RS256]);
        let listed = ["RS256", "ES256", "HS256", "none"].map(String::from);
        assert_eq!(
            allowed_algorithms(&listed),
            vec![Algorithm::RS256, Algorithm::ES256]
        );
        assert!(!allowed_algorithms(&["HS256".to_string()]).contains(&Algorithm::HS256));
    }

    #[test]
    fn the_key_must_match_the_kid_and_the_algorithm() {
        let s = set(serde_json::json!([rsa_key("a", Some("RS256"))]));
        assert!(pick_key(&s, Some("a"), Algorithm::RS256).is_some());
        // Another algorithm than the key's.
        assert!(pick_key(&s, Some("a"), Algorithm::RS512).is_none());
        // An unknown kid does not fall back to the only key.
        assert!(pick_key(&s, Some("b"), Algorithm::RS256).is_none());
        // No kid: the only key, when its algorithm fits.
        assert!(pick_key(&s, None, Algorithm::RS256).is_some());
        let two = set(serde_json::json!([rsa_key("a", None), rsa_key("b", None)]));
        assert!(pick_key(&two, None, Algorithm::RS256).is_none());
        assert!(pick_key(&two, Some("b"), Algorithm::PS256).is_some());
    }
}

#[cfg(test)]
mod login_url_tests {
    use super::*;

    const GOOGLE: &str = "312345678901234567";
    const MICROSOFT: &str = "312345678901234568";

    fn settings(ids: &[(&str, &str)]) -> OidcSettings {
        OidcSettings {
            issuer: "https://auth.example".into(),
            client_id: "ruum-web".into(),
            client_secret: None,
            redirect_uri: "https://ruum.example/v1/auth/callback".into(),
            scopes: vec!["openid".into(), "email".into(), "profile".into()],
            idp_ids: ids
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    /// Starts a login against a stub discovery document and returns the `scope` parameter of the
    /// authorization URL, with the URL's checked host and path.
    async fn scope_of(s: OidcSettings, hint: Option<&str>) -> String {
        let client = HttpOidcClient::new(s, None).unwrap();
        client
            .discovery
            .set(Discovery {
                issuer: "https://auth.example".into(),
                authorization_endpoint: "https://auth.example/oauth/v2/authorize".into(),
                token_endpoint: "https://auth.example/oauth/v2/token".into(),
                jwks_uri: "https://auth.example/oauth/v2/keys".into(),
                end_session_endpoint: None,
                id_token_signing_alg_values_supported: vec![],
            })
            .unwrap();
        let start = client
            .start_login(None, hint.map(str::to_string))
            .await
            .unwrap();
        let url = url::Url::parse(&start.authorize_url).unwrap();
        assert_eq!(url.host_str(), Some("auth.example"));
        assert_eq!(url.path(), "/oauth/v2/authorize");
        let scopes: Vec<String> = url
            .query_pairs()
            .filter(|(k, _)| k == "scope")
            .map(|(_, v)| v.into_owned())
            .collect();
        assert_eq!(scopes.len(), 1, "one scope parameter");
        scopes.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn a_configured_hint_adds_the_zitadel_idp_scope() {
        let both = settings(&[("google", GOOGLE), ("microsoft", MICROSOFT)]);
        assert_eq!(
            scope_of(both.clone(), Some("google")).await,
            format!("openid email profile urn:zitadel:iam:org:idp:id:{GOOGLE}")
        );
        assert_eq!(
            scope_of(both, Some("microsoft")).await,
            format!("openid email profile urn:zitadel:iam:org:idp:id:{MICROSOFT}")
        );
    }

    #[tokio::test]
    async fn without_an_id_zitadel_shows_its_choice() {
        let google_only = settings(&[("google", GOOGLE)]);
        assert_eq!(
            scope_of(google_only.clone(), Some("microsoft")).await,
            "openid email profile"
        );
        assert_eq!(
            scope_of(google_only.clone(), Some("passkey")).await,
            "openid email profile"
        );
        assert_eq!(scope_of(google_only, None).await, "openid email profile");
        assert_eq!(
            scope_of(settings(&[]), Some("google")).await,
            "openid email profile"
        );
    }

    #[tokio::test]
    async fn an_id_that_is_not_digits_never_reaches_the_url() {
        // from_env refuses such a value; the client checks again.
        for bad in [
            "1 openid offline_access",
            "1&prompt=none",
            "1%20x",
            "urn:zitadel:iam:org:project:id:zitadel:aud",
            "",
        ] {
            let scope = scope_of(settings(&[("google", bad)]), Some("google")).await;
            assert_eq!(scope, "openid email profile", "{bad:?}");
        }
    }
}
