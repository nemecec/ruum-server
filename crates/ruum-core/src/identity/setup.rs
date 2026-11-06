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
//! | `SMTP_URL` | `smtp://host:port` of the mail relay (Mailpit on the laptop). Without it mails stay in memory. |
//! | `RUUM_MAIL_FROM` | The sender address; default `no-reply@<host of RUUM_PUBLIC_URL>`. |
//! | `RUUM_PLAN_LIMITS` | The plan table as JSON (`tenants::PlanTable`); default the numbers of design section 4.3. |

use super::memory::MemoryOidcClient;
use super::oidc::HttpOidcClient;
use super::postgres::{PgArea, PgIdempotencyStore, PgIdentityStore};
use super::{JwtTokenIssuer, OidcSettings, TokenIssuer};
use crate::agents::postgres::{PgAgentStore, PgOAuthServer};
use crate::error::{Error, Result};
use crate::services::Services;
use crate::tenants::mail::{Mailer, MemoryMailer, SmtpMailer};
use crate::tenants::postgres::PgTenantStore;
use crate::tenants::{PlanTable, configure_plans};
use std::sync::Arc;

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
    /// Reads the settings of the table above. `public_url` is `RUUM_PUBLIC_URL`.
    pub fn from_env(public_url: &str) -> Self {
        let public_url = public_url.trim_end_matches('/').to_string();
        let oidc = match (var("RUUM_OIDC_ISSUER"), var("RUUM_OIDC_CLIENT_ID")) {
            (Some(issuer), Some(client_id)) => Some(OidcSettings {
                issuer,
                client_id,
                client_secret: var("RUUM_OIDC_CLIENT_SECRET"),
                redirect_uri: var("RUUM_OIDC_REDIRECT_URI")
                    .unwrap_or_else(|| format!("{public_url}/v1/auth/callback")),
                scopes: vec!["openid".into(), "email".into(), "profile".into()],
            }),
            _ => None,
        };
        Self {
            public_url,
            token_signing_key: var("RUUM_TOKEN_SIGNING_KEY"),
            oidc,
            smtp_url: var("SMTP_URL"),
            mail_from: var("RUUM_MAIL_FROM"),
            plan_limits: var("RUUM_PLAN_LIMITS"),
        }
    }
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
    let mailer: Arc<dyn Mailer> = match &settings.smtp_url {
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
    };
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
