//! The app gateway (card G-1a; `contracts/apps/gateway.openapi.yaml`; decision 0006-1;
//! `docs/STEP3.md` "How an app runs" rules 3 and 4, "Host names and slugs", "System lookups",
//! "Processes").
//!
//! Every request to an app origin goes through [`handle`]:
//!
//! 1. The host ([`host::parse`]: labels only) and the narrow system lookup ([`host::HostCache`]):
//!    unknown host, reserved or malformed label: the gateway's `404` page.
//! 2. What the deployment is ([`core_api::CoreCache::deployment`]): the environment, `access`,
//!    the tenant's public approval, whether it serves.
//! 3. Paths under `/.ruum/` belong to the gateway ([`session::gateway_path`]): the sign-in on
//!    the app origin, logout; the rest answers `404` and never reaches the app.
//! 4. The access check ([`check::check`]): `use` on live hosts, `open_preview` on previews, agent
//!    tokens on previews only, public apps while approved.
//! 5. The answer: a static file of the deployment ([`static_files`]) or, for the cleaning cases,
//!    the fixed test upstream ([`proxy`]), with a fresh `Ruum-User` token ([`token`]), cleaned
//!    both ways. Card G-1b adds the proxy to a running sandbox.
//!
//! The gateway holds **no tenant keys and no general system access**: the host lookup (its
//! database role may only execute `ruum_app_host_lookup`), the core's small internal API
//! ([`core_api`]; the core runs the SpiceDB checks and holds the pseudonym keys of the pairwise
//! subjects), the user-token signing key, and a read-only object store credential for the static
//! sites.
//!
//! The module tree of `docs/STEP3.md` "Shared files": `host`, `session`, `check`, `token`,
//! `static_files`, `proxy`, `telemetry` (G-2), `reports` (G-4), `domains` (W-5).

pub mod check;
pub mod core_api;
pub mod domains;
pub mod host;
pub mod pages;
pub mod proxy;
pub mod reports;
pub mod session;
pub mod static_files;
pub mod telemetry;
pub mod token;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use futures::TryStreamExt;
use host::{AppHost, HostCache, Resolved};
use proxy::Frame;
use ruum_core::apps::static_site::StaticSites;
use ruum_core::identity::app_sign_in::wire::{DeploymentInfo, DeploymentQuery};
use ruum_core::identity::app_sign_in::{AppBinding, AppEnv};
use std::sync::Arc;
use token::{TokenSubject, UserTokenSigner};
use uuid::Uuid;

/// The settings of a gateway.
#[derive(Clone, Debug)]
pub struct Config {
    /// The registered domain of app origins (`RUUM_APPS_DOMAIN`; `ruumapps.localhost` on the
    /// laptop).
    pub apps_domain: String,
    /// The origin of the platform, where `/v1/auth/login` is (`RUUM_GATEWAY_PLATFORM_URL`).
    pub platform_url: String,
    /// The Ruum UI origin: the only `frame-ancestors` of previews (`RUUM_GATEWAY_UI_ORIGIN`).
    pub ui_origin: String,
}

/// What a request's host resolved to.
#[derive(Clone, Debug)]
pub struct Site {
    pub host: AppHost,
    /// The host name (lower case, no port).
    pub name: String,
    pub resolved: Resolved,
    pub info: DeploymentInfo,
    pub binding: AppBinding,
    ui_origin: String,
}

impl Site {
    /// The `frame-ancestors` rule of the site's environment.
    pub fn frame(&self) -> Frame {
        match self.info.environment {
            AppEnv::Live => Frame::Live,
            AppEnv::Preview => Frame::Preview,
        }
    }

    /// How the gateway's pages of this site look.
    pub fn look(&self) -> pages::Look<'_> {
        (self.frame(), &self.ui_origin)
    }

    /// The audience of the site's user tokens.
    pub fn audience(&self) -> String {
        match self.info.environment {
            AppEnv::Live => token::live_aud(self.resolved.app_id),
            AppEnv::Preview => {
                token::preview_aud(self.resolved.app_id, self.resolved.deployment_id)
            }
        }
    }
}

/// A running gateway: its settings and its parts.
pub struct Gateway {
    pub config: Config,
    pub hosts: HostCache,
    pub core: core_api::CoreCache,
    pub tokens: UserTokenSigner,
    pub sites: StaticSites,
    pub upstreams: proxy::TestUpstreams,
    pub visits: session::Visits,
    http: reqwest::Client,
}

impl Gateway {
    pub fn new(
        config: Config,
        hosts: Arc<dyn host::HostLookup>,
        core: Arc<dyn core_api::CoreApi>,
        tokens: UserTokenSigner,
        sites: StaticSites,
    ) -> Self {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("an HTTP client without TLS settings builds");
        Self {
            config,
            hosts: HostCache::new(hosts),
            core: core_api::CoreCache::new(core),
            tokens,
            sites,
            upstreams: proxy::TestUpstreams::default(),
            visits: session::Visits::default(),
            http,
        }
    }
}

/// The router of a gateway: every request goes to [`handle`].
pub fn router(gw: Arc<Gateway>) -> Router {
    Router::new().fallback(handle).with_state(gw)
}

/// The look of an answer before the environment is known.
fn no_site(gw: &Gateway) -> pages::Look<'_> {
    (Frame::Live, &gw.config.ui_origin)
}

/// Resolves the host of a request (steps 1 and 2 of the crate docs). `Err` is the answer.
async fn site_of(gw: &Gateway, raw_host: &str) -> Result<Site, Box<Response>> {
    let Some(app_host) = host::parse(raw_host, &gw.config.apps_domain) else {
        return Err(Box::new(pages::not_found(no_site(gw))));
    };
    let resolved = match gw.hosts.lookup(&app_host).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(Box::new(pages::not_found(no_site(gw)))),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "the host lookup failed");
            return Err(Box::new(pages::unavailable(no_site(gw))));
        }
    };
    let info = match gw
        .core
        .deployment(&DeploymentQuery {
            tenant_id: resolved.tenant_id,
            app_id: resolved.app_id,
            deployment_id: resolved.deployment_id,
        })
        .await
    {
        Ok(Some(i)) if i.serving => i,
        Ok(_) => return Err(Box::new(pages::not_found(no_site(gw)))),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "the deployment check failed");
            return Err(Box::new(pages::unavailable(no_site(gw))));
        }
    };
    // A preview host names a preview, a live host the live deployment.
    let preview = app_host.short_id.is_some();
    if preview != (info.environment == AppEnv::Preview) {
        return Err(Box::new(pages::not_found(no_site(gw))));
    }
    let binding = AppBinding::of(
        resolved.tenant_id,
        resolved.app_id,
        info.environment,
        resolved.deployment_id,
    );
    Ok(Site {
        name: app_host.name(&gw.config.apps_domain.trim_matches('.').to_ascii_lowercase()),
        host: app_host,
        resolved,
        info,
        binding,
        ui_origin: gw.config.ui_origin.clone(),
    })
}

/// Every request of an app origin (see the crate docs).
pub async fn handle(State(gw): State<Arc<Gateway>>, req: Request) -> Response {
    // One host only: two `Host` headers, or an absolute-form target whose authority is not the
    // `Host`, get 400 (the gateway decides on the request it checked; RFC 9112 3.2).
    let mut hosts = req.headers().get_all(header::HOST).iter();
    let host_header = hosts
        .next()
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if hosts.next().is_some() {
        return pages::bad_request(no_site(&gw));
    }
    let authority = req.uri().authority().map(|a| a.to_string());
    let raw_host = match (host_header, authority) {
        (Some(h), Some(a)) if host::normalize(&h) != host::normalize(&a) => {
            return pages::bad_request(no_site(&gw));
        }
        (Some(h), _) => h,
        (None, Some(a)) => a,
        (None, None) => String::new(),
    };
    let site = match site_of(&gw, &raw_host).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    let path = req.uri().path().to_string();
    if path == "/.ruum" || path.starts_with("/.ruum/") {
        return session::gateway_path(&gw, &site, req.method(), req.uri(), req.headers()).await;
    }
    let (visitor, strip_authorization) = match check::check(&gw, &site, req.headers()).await {
        check::Outcome::Pass {
            visitor,
            strip_authorization,
        } => (visitor, strip_authorization),
        check::Outcome::SignIn => {
            if req.method() == Method::GET || req.method() == Method::HEAD {
                let target = req
                    .uri()
                    .path_and_query()
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_else(|| "/".into());
                let mut r = Response::new(Body::empty());
                *r.status_mut() = StatusCode::FOUND;
                if let Ok(v) = HeaderValue::from_str(&session::sign_in_location(&target)) {
                    r.headers_mut().insert(header::LOCATION, v);
                }
                r.headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                proxy::security_headers(r.headers_mut(), site.frame(), &gw.config.ui_origin);
                return r;
            }
            return pages::unauthorized(site.look());
        }
        check::Outcome::Unauthorized => return pages::unauthorized(site.look()),
        check::Outcome::Forbidden => return pages::forbidden(site.look()),
        check::Outcome::Unavailable => return pages::unavailable(site.look()),
    };
    let (visit, new_visit) = session::visit_of(req.headers());
    let mut resp = match gw.upstreams.get(site.resolved.deployment_id) {
        Some(url) => forward(&gw, &site, &visitor, &visit, strip_authorization, &url, req).await,
        None => {
            let (parts, _) = req.into_parts();
            static_files::serve(&gw, &site, &parts.method, &path, &parts.headers).await
        }
    };
    if new_visit && let Ok(v) = HeaderValue::from_str(&session::visit_cookie(&visit)) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// The subject of the user token of a visitor.
fn subject_of(visitor: &check::Visitor, visit: &str) -> TokenSubject {
    match visitor {
        check::Visitor::Anonymous => TokenSubject {
            sub: session::anonymous_subject(visit),
            actor: "anonymous",
            on_behalf_of: None,
            roles: Vec::new(),
        },
        check::Visitor::User { info, .. } => TokenSubject {
            sub: info.sub.clone(),
            actor: "user",
            on_behalf_of: None,
            roles: info.roles.clone(),
        },
        check::Visitor::Agent { sub, on_behalf_of } => TokenSubject {
            sub: sub.clone(),
            actor: "agent",
            on_behalf_of: Some(on_behalf_of.clone()),
            roles: Vec::new(),
        },
    }
}

/// The largest request body that the fixed test upstream takes.
pub const MAX_FORWARD_BODY: usize = 16 * 1024 * 1024;

/// Forwards a request that passed the check to `upstream`, cleaned both ways.
async fn forward(
    gw: &Gateway,
    site: &Site,
    visitor: &check::Visitor,
    visit: &str,
    strip_authorization: bool,
    upstream: &str,
    req: Request,
) -> Response {
    let (parts, body) = req.into_parts();
    // The fixed test upstream takes the whole body (G-1b streams to sandboxes). The body was
    // read by one framing (hyper refuses conflicting lengths; chunked wins over a length), and
    // the forwarded request has its own length, never the client's framing headers.
    let body = match axum::body::to_bytes(body, MAX_FORWARD_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return pages::page(
                StatusCode::PAYLOAD_TOO_LARGE,
                "This request is too large.",
                site.frame(),
                &gw.config.ui_origin,
            );
        }
    };
    let subject = subject_of(visitor, visit);
    let claims = gw.tokens.claims(
        site.audience(),
        site.info.environment.as_str(),
        &site.host.tenant_slug,
        &subject,
    );
    let Ok(user_token) = gw.tokens.sign(&claims) else {
        return pages::unavailable(site.look());
    };
    let (traceparent, trace_id) = proxy::new_traceparent();
    let session = match visitor {
        check::Visitor::User { session, .. } => Some(session.as_str()),
        _ => None,
    };
    gw.visits.record(visit, session, trace_id);
    let mut headers = proxy::clean_request(&parts.headers, strip_authorization);
    headers.remove(header::CONTENT_LENGTH);
    proxy::add_gateway_headers(&mut headers, &user_token, Uuid::now_v7(), &traceparent);
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let url = format!("{upstream}{target}");
    let sent = gw
        .http
        .request(parts.method.clone(), url)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let up = match sent {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e.without_url(), "the upstream did not answer");
            return pages::page(
                StatusCode::BAD_GATEWAY,
                "The app did not answer.",
                site.frame(),
                &gw.config.ui_origin,
            );
        }
    };
    let status = up.status();
    let headers = proxy::clean_response(up.headers(), site.frame(), &gw.config.ui_origin);
    let stream = up.bytes_stream().map_err(std::io::Error::other);
    let mut r = Response::new(Body::from_stream(stream));
    *r.status_mut() = status;
    *r.headers_mut() = headers;
    r
}

/// Serves `gw` on `listener` until the future ends.
pub async fn serve(listener: tokio::net::TcpListener, gw: Arc<Gateway>) -> std::io::Result<()> {
    axum::serve(listener, router(gw)).await
}
