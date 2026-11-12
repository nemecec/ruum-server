//! Wiring of the identity, tenants and agents area for the server: the real token key, the OIDC
//! client of Zitadel, the mailer, the plan table, and the Postgres stores when there is a
//! database.
//!
//! Settings (environment):
//!
//! | Name | What |
//! |---|---|
//! | `RUUM_TOKEN_SIGNING_KEY` | The Ed25519 seed (32 bytes, base64url) of the token key. Without it the server makes a new key at start, so tokens end at a restart. |
//! | `RUUM_OIDC_ISSUER`, `RUUM_OIDC_CLIENT_ID`, `RUUM_OIDC_CLIENT_SECRET` | The OIDC client (Zitadel). Without issuer and client ID, sign-in uses the fake provider of the in-memory services. |
//! | `RUUM_OIDC_REDIRECT_URI` | The callback; default `<RUUM_PUBLIC_URL>/v1/auth/callback`. |
//! | `RUUM_OIDC_IDP_GOOGLE`, `RUUM_OIDC_IDP_MICROSOFT` | The Zitadel IdP ID (digits) of Google and Microsoft. With it, `/v1/auth/login?idp=google` (or `microsoft`) goes straight to that provider; without it, Zitadel shows its choice. A value that is not an ID stops the server at start. |
//! | `SMTP_URL` | `smtp://host:port` of the mail relay (Mailpit on the laptop). Without it mails stay in memory. |
//! | `RUUM_MAIL_FROM` | The sender address; default `no-reply@<host of RUUM_PUBLIC_URL>`. |
//! | `RUUM_PLAN_LIMITS` | The plan table as JSON (`tenants::PlanTable`); default the numbers of design section 4.3. Each plan may hold `databases` (C-21): `{"postgres": {"count": 1, "size_bytes": 262144000}, "sqlite": {...}, "branches": null}`, where `null` is no limit; `count` and `branches` are per member on `team`. A plan without `databases` keeps the defaults of section 4.3. |

use super::memory::MemoryOidcClient;
use super::oidc::HttpOidcClient;
use super::postgres::{PgArea, PgIdempotencyStore, PgIdentityStore};
use super::{JwtTokenIssuer, OidcSettings, TokenIssuer, zitadel_idp_id};
use crate::agents::postgres::{PgAgentStore, PgOAuthServer};
use crate::error::{Error, Result};
use crate::services::Services;
use crate::tenants::mail::{Mailer, MemoryMailer, SmtpMailer};
use crate::tenants::postgres::PgTenantStore;
use crate::tenants::{PlanTable, configure_plans};
use std::sync::Arc;

/// The sign-in hints that can name a Zitadel IdP, with the setting that holds its ID.
pub const IDP_HINT_SETTINGS: &[(&str, &str)] = &[
    ("google", "RUUM_OIDC_IDP_GOOGLE"),
    ("microsoft", "RUUM_OIDC_IDP_MICROSOFT"),
];

/// Reads the IdP IDs of [`IDP_HINT_SETTINGS`] with `get`. A value that is not a Zitadel ID is an
/// error.
fn idp_ids(
    get: impl Fn(&str) -> Option<String>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut ids = std::collections::BTreeMap::new();
    for (hint, name) in IDP_HINT_SETTINGS {
        if let Some(v) = get(name) {
            let id = zitadel_idp_id(&v).ok_or_else(|| {
                Error::Internal(format!("{name} must be a Zitadel IdP ID (digits only)"))
            })?;
            ids.insert((*hint).to_string(), id.to_string());
        }
    }
    Ok(ids)
}

/// The settings of the area.
#[derive(Clone, Debug, Default)]
pub struct IdentitySettings {
    /// The public origin of the API and the web UI, for example `https://ruum.localhost`.
    pub public_url: String,
    pub token_signing_key: Option<String>,
    pub oidc: Option<OidcSettings>,
    pub smtp_url: Option<String>,
    pub mail_from: Option<String>,
    pub plan_limits: Option<String>,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl IdentitySettings {
    /// Reads the settings of the table above. `public_url` is `RUUM_PUBLIC_URL`. An IdP ID that
    /// is not valid is an error.
    pub fn from_env(public_url: &str) -> Result<Self> {
        let public_url = public_url.trim_end_matches('/').to_string();
        let oidc = match (var("RUUM_OIDC_ISSUER"), var("RUUM_OIDC_CLIENT_ID")) {
            (Some(issuer), Some(client_id)) => Some(OidcSettings {
                issuer,
                client_id,
                client_secret: var("RUUM_OIDC_CLIENT_SECRET"),
                redirect_uri: var("RUUM_OIDC_REDIRECT_URI")
                    .unwrap_or_else(|| format!("{public_url}/v1/auth/callback")),
                scopes: vec!["openid".into(), "email".into(), "profile".into()],
                idp_ids: idp_ids(var)?,
            }),
            _ => None,
        };
        Ok(Self {
            public_url,
            token_signing_key: var("RUUM_TOKEN_SIGNING_KEY"),
            oidc,
            smtp_url: var("SMTP_URL"),
            mail_from: var("RUUM_MAIL_FROM"),
            plan_limits: var("RUUM_PLAN_LIMITS"),
        })
    }
}

/// The mailer of the settings: SMTP to `SMTP_URL` (Mailpit on the laptop), else in memory.
/// The server's mail outbox worker (`mailing::outbox::run_worker`) sends with it too.
pub fn mailer(settings: &IdentitySettings) -> Result<Arc<dyn Mailer>> {
    let origin = settings.public_url.as_str();
    Ok(match &settings.smtp_url {
        Some(url) => {
            let host = url::Url::parse(origin)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| "ruum.localhost".into());
            let from = settings
                .mail_from
                .clone()
                .unwrap_or_else(|| format!("no-reply@{host}"));
            Arc::new(SmtpMailer::from_url(url, &from)?)
        }
        None => Arc::new(MemoryMailer::default()),
    })
}

/// Puts the area into `services`: the OIDC client, and with `pool` the token issuer of the
/// configured key and the Postgres stores (they write relationships through `services.authz`,
/// so call this after it is set). Without `pool` the in-memory stores of `Services::in_memory`
/// stay.
pub fn install(
    services: &mut Services,
    settings: &IdentitySettings,
    pool: Option<sqlx::PgPool>,
) -> Result<()> {
    if let Some(json) = &settings.plan_limits {
        let table: PlanTable = serde_json::from_str(json)
            .map_err(|e| Error::Internal(format!("RUUM_PLAN_LIMITS: {e}")))?;
        configure_plans(table);
    }
    let origin = settings.public_url.as_str();
    let tokens: Arc<dyn TokenIssuer> = match &settings.token_signing_key {
        Some(seed) => Arc::new(JwtTokenIssuer::from_base64_seed(origin, origin, seed)?),
        None => {
            tracing::warn!(
                "RUUM_TOKEN_SIGNING_KEY is not set; a new token key is made, so tokens end at a restart"
            );
            Arc::new(JwtTokenIssuer::generate(origin, origin))
        }
    };
    let mailer = mailer(settings)?;
    services.oidc = match &settings.oidc {
        Some(o) => Arc::new(HttpOidcClient::new(o.clone(), Some(format!("{origin}/")))?),
        None => Arc::new(MemoryOidcClient::default()),
    };
    match pool {
        Some(pool) => {
            services.tokens = tokens.clone();
            let area = PgArea {
                pool: pool.clone(),
                tokens,
                authz: Some(services.authz.clone()),
                mailer,
                public_url: origin.to_string(),
            };
            services.identity = Arc::new(PgIdentityStore::new(area.clone()));
            services.tenants = Arc::new(PgTenantStore::new(area.clone()));
            services.agents = Arc::new(PgAgentStore::new(area.clone()));
            services.oauth = Arc::new(PgOAuthServer::new(area));
            services.idempotency = Arc::new(PgIdempotencyStore::new(pool));
        }
        None => {
            // The in-memory services keep their own token issuer (origin
            // `https://ruum.localhost`) and stores: other in-memory stores already hold them.
            if origin != services.tokens.audience() {
                tracing::warn!(
                    origin,
                    "in-memory services sign tokens for https://ruum.localhost only"
                );
            }
            let _ = (tokens, mailer);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn idp_ids_are_read_per_hint() {
        let ids = idp_ids(get(&[
            ("RUUM_OIDC_IDP_GOOGLE", "312345678901234567"),
            ("RUUM_OIDC_IDP_MICROSOFT", " 312345678901234568 "),
        ]))
        .unwrap();
        assert_eq!(ids["google"], "312345678901234567");
        assert_eq!(ids["microsoft"], "312345678901234568");
        let only = idp_ids(get(&[("RUUM_OIDC_IDP_GOOGLE", "1")])).unwrap();
        assert_eq!(only.len(), 1);
        assert!(idp_ids(get(&[])).unwrap().is_empty());
    }

    #[test]
    fn an_idp_id_that_is_not_digits_stops_the_start() {
        for bad in ["abc", "123 openid", "12%2034", "1-2", "\u{0661}"] {
            let e = idp_ids(get(&[("RUUM_OIDC_IDP_MICROSOFT", bad)])).unwrap_err();
            assert!(
                e.to_string().contains("RUUM_OIDC_IDP_MICROSOFT"),
                "{bad}: {e}"
            );
        }
        assert!(idp_ids(get(&[("RUUM_OIDC_IDP_GOOGLE", &"9".repeat(33))])).is_err());
    }
}
