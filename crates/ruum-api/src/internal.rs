//! The internal router: endpoints for the edge inside the cluster, never for the Internet.
//!
//! `ruum-server` serves it on its own listener, `RUUM_INTERNAL_BIND` (off when not set). No
//! public route of the edge points at that listener, and the network policy of staging lets only
//! the edge reach it (review 1 B-6).
//!
//! - `GET /internal/tls-ask?domain=<host>`: the `ask` endpoint of Caddy's on-demand TLS. It
//!   answers `200` only for `<tenant>.<content host>` where the tenant exists, and `404` for every
//!   other name, so Caddy gets no certificate for a name that nobody uses. Caddy fails closed: an
//!   error or a timeout also means no certificate. New names are rate limited
//!   ([`TlsAskConfig::new_hosts_per_hour`]), so even many real tenants cannot use up the CA's
//!   limits in a burst; a name that was allowed once stays allowed without counting again.

use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use ruum_core::blobs::signed::tenant_of_host;
use serde::Deserialize;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Settings of the `ask` endpoint.
#[derive(Clone, Debug)]
pub struct TlsAskConfig {
    /// The host of the user content domain, for example `content.staging.ruumapps.net`.
    pub content_host: String,
    /// At most this many names that were not allowed before get `200` per hour
    /// (`RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR`, default 20). Over the limit: `429`.
    pub new_hosts_per_hour: usize,
}

impl TlsAskConfig {
    /// The settings for a content base URL such as `https://content.staging.ruumapps.net`.
    pub fn for_content_url(content_url: &str, new_hosts_per_hour: usize) -> Self {
        let host = url::Url::parse(content_url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_else(|| content_url.trim_end_matches('/').to_ascii_lowercase());
        Self {
            content_host: host,
            new_hosts_per_hour,
        }
    }
}

#[derive(Default)]
struct Window {
    /// Names that got `200`.
    allowed: HashSet<String>,
    /// When each new name got `200`, in the last hour.
    recent: VecDeque<Instant>,
}

#[derive(Clone)]
struct AskState {
    app: AppState,
    config: TlsAskConfig,
    window: Arc<Mutex<Window>>,
}

#[derive(Deserialize)]
struct AskQuery {
    domain: Option<String>,
}

const HOUR: Duration = Duration::from_secs(3600);

/// The internal router (see the module docs).
pub fn router(app: AppState, config: TlsAskConfig) -> Router {
    Router::new()
        .route("/internal/tls-ask", get(tls_ask))
        .with_state(AskState {
            app,
            config,
            window: Arc::default(),
        })
}

async fn tls_ask(State(s): State<AskState>, Query(q): Query<AskQuery>) -> StatusCode {
    let Some(domain) = q
        .domain
        .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
    else {
        return StatusCode::BAD_REQUEST;
    };
    let Some(label) = domain
        .strip_suffix(&s.config.content_host)
        .and_then(|rest| rest.strip_suffix('.'))
    else {
        return StatusCode::NOT_FOUND;
    };
    // Exactly one label in front of the content host, and it is a tenant ID.
    if label.is_empty() || label.contains('.') {
        return StatusCode::NOT_FOUND;
    }
    let Some(tenant) = tenant_of_host(label) else {
        return StatusCode::NOT_FOUND;
    };
    {
        let w = s.window.lock().expect("ask window lock");
        if w.allowed.contains(&domain) {
            return StatusCode::OK;
        }
    }
    if s.app.services.tenants.find(tenant).await.is_err() {
        return StatusCode::NOT_FOUND;
    }
    let mut w = s.window.lock().expect("ask window lock");
    if w.allowed.contains(&domain) {
        return StatusCode::OK;
    }
    let now = Instant::now();
    while w
        .recent
        .front()
        .is_some_and(|t| now.duration_since(*t) >= HOUR)
    {
        w.recent.pop_front();
    }
    if w.recent.len() >= s.config.new_hosts_per_hour {
        tracing::warn!(
            target: "ruum::security",
            event = "tls.ask_rate_limited",
            "too many new content hosts in the last hour; no certificate"
        );
        return StatusCode::TOO_MANY_REQUESTS;
    }
    w.recent.push_back(now);
    w.allowed.insert(domain);
    StatusCode::OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ruum_core::identity::VerifiedLogin;
    use tower::ServiceExt;

    async fn ask(app: &Router, domain: &str) -> StatusCode {
        let uri = format!("/internal/tls-ask?domain={domain}");
        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        app.clone().oneshot(req).await.unwrap().status()
    }

    /// A new user through the fake sign-in; their personal tenant exists.
    async fn tenant(state: &AppState, email: &str) -> ruum_core::TenantId {
        let user = state
            .services
            .identity
            .upsert_from_login(&VerifiedLogin {
                issuer: "https://issuer.example".into(),
                subject: email.into(),
                email: Some(email.into()),
                email_verified: true,
                name: None,
                id_token: None,
            })
            .await
            .unwrap();
        state
            .services
            .tenants
            .find(user.home_tenant_id)
            .await
            .unwrap();
        user.home_tenant_id
    }

    #[tokio::test]
    async fn only_existing_tenant_hosts_get_a_certificate() {
        let (_, _, state) = crate::routes::auth::tests::app();
        let t = tenant(&state, "owner@example.test").await;
        let app = router(
            state,
            TlsAskConfig::for_content_url("https://content.staging.ruumapps.net", 20),
        );
        let host = format!("{t}.content.staging.ruumapps.net");
        assert_eq!(ask(&app, &host).await, StatusCode::OK);
        assert_eq!(ask(&app, &host.to_uppercase()).await, StatusCode::OK);
        assert_eq!(ask(&app, &format!("{host}.")).await, StatusCode::OK);
        let unknown = format!("{}.content.staging.ruumapps.net", uuid::Uuid::now_v7());
        assert_eq!(ask(&app, &unknown).await, StatusCode::NOT_FOUND);
        for bad in [
            "content.staging.ruumapps.net",
            "x.content.staging.ruumapps.net",
            "staging.ruum.space",
            "evil.example",
            &format!("a.{t}.content.staging.ruumapps.net"),
            &format!("{t}.content.staging.ruumapps.net.evil.example"),
            &format!("{t}content.staging.ruumapps.net"),
        ] {
            assert_eq!(ask(&app, bad).await, StatusCode::NOT_FOUND, "{bad}");
        }
        let req = Request::builder()
            .uri("/internal/tls-ask")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn new_hosts_are_rate_limited_and_known_hosts_are_not() {
        let (_, _, state) = crate::routes::auth::tests::app();
        let a = tenant(&state, "a@example.test").await;
        let b = tenant(&state, "b@example.test").await;
        let app = router(
            state,
            TlsAskConfig::for_content_url("https://content.staging.ruumapps.net/", 1),
        );
        let ha = format!("{a}.content.staging.ruumapps.net");
        let hb = format!("{b}.content.staging.ruumapps.net");
        assert_eq!(ask(&app, &ha).await, StatusCode::OK);
        assert_eq!(ask(&app, &hb).await, StatusCode::TOO_MANY_REQUESTS);
        // The host allowed before stays allowed.
        assert_eq!(ask(&app, &ha).await, StatusCode::OK);
    }

    #[test]
    fn the_content_host_comes_from_the_url() {
        let c = TlsAskConfig::for_content_url("https://Content.Staging.ruumapps.net/", 5);
        assert_eq!(c.content_host, "content.staging.ruumapps.net");
    }
}
