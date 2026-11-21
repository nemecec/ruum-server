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
//! 5. The answer: a static file of the deployment ([`static_files`]), or for a sandbox
//!    deployment (card G-1b) the proxy to its instance: the core says where it runs and wakes
//!    it while the gateway holds the request ([`sandbox`]), and [`proxy::send`] streams the
//!    request and the answer, cleaned both ways, with a fresh `Ruum-User` token ([`token`]);
//!    WebSockets pass the same check and are checked again while open. The fixed test upstream
//!    of the cleaning cases goes through the same [`proxy::send`]. When the instance does not
//!    run, the gateway answers with its own status pages ([`pages`]): starting (`503` with
//!    `Retry-After`), not running or crashed (`502`), timeout (`504`).
//!
//! The gateway holds **no tenant keys and no general system access**: the host lookup (its
//! database role may only execute `ruum_app_host_lookup`), the core's small internal API
//! ([`core_api`]; the core runs the SpiceDB checks and holds the pseudonym keys of the pairwise
//! subjects) and the user-token signing key. It holds no key of the static sites' bucket: the
//! core gives it a short-lived read link per file of a serving deployment ([`static_files`];
//! decision 0008-4).
//!
//! The module tree of `docs/STEP3.md` "Shared files": `host`, `session`, `check`, `token`,
//! `static_files`, `proxy`, `sandbox` (G-1b), `telemetry` (G-2), `reports` (G-4), `domains`
//! (W-5).

pub mod check;
pub mod core_api;
pub mod domains;
pub mod host;
pub mod pages;
pub mod proxy;
pub mod reports;
pub mod sandbox;
pub mod session;
pub mod static_files;
pub mod telemetry;
pub mod token;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use host::{AppHost, HostCache, Resolved};
use proxy::{Frame, Outgoing, Sent};
use ruum_core::identity::app_sign_in::wire::{DeploymentInfo, DeploymentQuery, UpstreamQuery};
use ruum_core::identity::app_sign_in::{AppBinding, AppEnv};
use sandbox::{HoldConfig, Route, SandboxHold, Sandboxes};
use std::sync::Arc;
use token::{TokenSubject, UserTokenSigner};

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
    /// Static files: through the core (decision 0008-4).
    pub statics: Arc<dyn static_files::StaticSource>,
    pub upstreams: proxy::TestUpstreams,
    pub visits: session::Visits,
    /// The core's answers where sandboxes run, and the hold of cold starts (card G-1b).
    pub sandboxes: SandboxHold,
    /// The settings of the proxy to an upstream.
    pub proxy: proxy::ProxyConfig,
    upstream_http: reqwest::Client,
}

impl Gateway {
    pub fn new(
        config: Config,
        hosts: Arc<dyn host::HostLookup>,
        core: Arc<dyn core_api::CoreApi>,
        tokens: UserTokenSigner,
        statics: Arc<dyn static_files::StaticSource>,
    ) -> Self {
        let proxy = proxy::ProxyConfig::default();
        Self {
            config,
            hosts: HostCache::new(hosts),
            core: core_api::CoreCache::new(core),
            tokens,
            statics,
            upstreams: proxy::TestUpstreams::default(),
            visits: session::Visits::default(),
            sandboxes: SandboxHold::new(Arc::new(sandbox::NoSandboxes), HoldConfig::default()),
            upstream_http: proxy::client(&proxy),
            proxy,
        }
    }

    /// Asks `sandboxes` where sandbox deployments run, with the hold limits of `hold`. Without
    /// it every sandbox deployment answers the `502` page.
    pub fn with_sandboxes(mut self, sandboxes: Arc<dyn Sandboxes>, hold: HoldConfig) -> Self {
        self.sandboxes = SandboxHold::new(sandboxes, hold);
        self
    }

    /// The settings of the proxy (timeouts, the body limit, WebSockets).
    pub fn with_proxy(mut self, proxy: proxy::ProxyConfig) -> Self {
        self.upstream_http = proxy::client(&proxy);
        self.proxy = proxy;
        self
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
pub(crate) async fn site_of(gw: &Gateway, raw_host: &str) -> Result<Site, Box<Response>> {
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
        check::Outcome::BadRequest => return pages::bad_request(site.look()),
    };
    let (visit, new_visit) = session::visit_of(req.headers());
    let from = proxy::Sender {
        visitor: &visitor,
        visit: &visit,
        strip_authorization,
    };
    let mut resp = match gw.upstreams.get(site.resolved.deployment_id) {
        Some(url) => forward_fixed(&gw, &site, &from, &url, req).await,
        None if site.info.sandbox => forward_sandbox(&gw, &site, &from, req).await,
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
pub(crate) fn subject_of(visitor: &check::Visitor, visit: &str) -> TokenSubject {
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

/// The fixed test upstream of the cleaning cases: the same [`proxy::send`] as a sandbox.
async fn forward_fixed(
    gw: &Arc<Gateway>,
    site: &Site,
    from: &proxy::Sender<'_>,
    upstream: &str,
    req: Request,
) -> Response {
    let out = match Outgoing::new(req, site, gw.proxy.max_body) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    match proxy::send(gw, site, from, out, upstream).await {
        Sent::Answer(r) => r,
        Sent::NoConnection(_) => pages::not_running(site.look()),
    }
}

/// The query of a site's sandbox: only the values the gateway resolved from the host.
fn upstream_query(site: &Site) -> UpstreamQuery {
    UpstreamQuery {
        tenant_id: site.resolved.tenant_id,
        app_id: site.resolved.app_id,
        deployment_id: site.resolved.deployment_id,
        environment: site.info.environment,
    }
}

/// A request of a sandbox deployment: where the core says its instance runs, held while it
/// starts (see [`sandbox`]).
async fn forward_sandbox(
    gw: &Arc<Gateway>,
    site: &Site,
    from: &proxy::Sender<'_>,
    req: Request,
) -> Response {
    let q = upstream_query(site);
    let mut out = match Outgoing::new(req, site, gw.proxy.max_body) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    // A second try only when the first address took no connection and nothing was sent. Both
    // tries share one hold (kimi-run12-g1b 2: the second one may wait for the app to listen).
    let deadline = tokio::time::Instant::now() + gw.sandboxes.config.hold;
    for _ in 0..2 {
        let url = match gw.sandboxes.route_until(&q, deadline).await {
            Route::Ready(u) => u,
            Route::Busy | Route::Starting => return pages::starting(site.look()),
            Route::NotRunning => return pages::not_running(site.look()),
            Route::Crashed => return pages::crashed(site.look()),
            Route::Unavailable => return pages::unavailable(site.look()),
        };
        match proxy::send(gw, site, from, out, &url).await {
            Sent::Answer(r) => return r,
            Sent::NoConnection(back) => {
                gw.sandboxes.forget(&q);
                match back {
                    Some(o) => out = o,
                    None => return pages::not_running(site.look()),
                }
            }
        }
    }
    pages::not_running(site.look())
}

/// Whether a recheck found the visitor of the handshake (review-g1b 1): the same person with
/// the same app roles (in any order), the same agent acting for the same person, or anonymous
/// again. These are the values of the handshake's `Ruum-User` token ([`subject_of`]), which the
/// app keeps acting on while the WebSocket is open.
pub(crate) fn same_visitor(then: &check::Visitor, now: &check::Visitor) -> bool {
    use check::Visitor::{Agent, Anonymous, User};
    match (then, now) {
        (Anonymous, Anonymous) => true,
        (User { info: a, .. }, User { info: b, .. }) => {
            let roles = |r: &[String]| {
                let mut v = r.to_vec();
                v.sort();
                v.dedup();
                v
            };
            a.sub == b.sub && roles(&a.roles) == roles(&b.roles)
        }
        (
            Agent {
                sub: a,
                on_behalf_of: x,
            },
            Agent {
                sub: b,
                on_behalf_of: y,
            },
        ) => a == b && x == y,
        _ => false,
    }
}

/// Whether an open WebSocket of `site` may stay open: the host still resolves to the same
/// deployment, which serves, and the request's credentials still pass the check as the same
/// `visitor` as at the handshake ([`same_visitor`]; the answers come through the caches of at
/// most 3 s). Any error closes it (fail closed).
pub(crate) async fn still_allowed(
    gw: &Gateway,
    site: &Site,
    headers: &HeaderMap,
    visitor: &check::Visitor,
) -> bool {
    let Ok(now) = site_of(gw, &site.name).await else {
        return false;
    };
    if now.resolved != site.resolved {
        return false;
    }
    match check::check(gw, &now, headers).await {
        check::Outcome::Pass { visitor: again, .. } => same_visitor(visitor, &again),
        _ => false,
    }
}

/// Serves `gw` on `listener` until the future ends.
pub async fn serve(listener: tokio::net::TcpListener, gw: Arc<Gateway>) -> std::io::Result<()> {
    axum::serve(listener, router(gw)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use check::Visitor;
    use ruum_core::identity::app_sign_in::wire::SessionInfo;

    fn user(sub: &str, roles: &[&str]) -> Visitor {
        Visitor::User {
            info: SessionInfo {
                sub: sub.into(),
                allowed: true,
                roles: roles.iter().map(|r| r.to_string()).collect(),
            },
            session: "s".into(),
        }
    }

    fn agent(sub: &str, on_behalf_of: &str) -> Visitor {
        Visitor::Agent {
            sub: sub.into(),
            on_behalf_of: on_behalf_of.into(),
        }
    }

    #[test]
    fn a_recheck_must_find_the_visitor_of_the_handshake() {
        // The same visitor (roles in another order).
        assert!(same_visitor(&Visitor::Anonymous, &Visitor::Anonymous));
        assert!(same_visitor(
            &user("p1", &["member", "approver"]),
            &user("p1", &["approver", "member"])
        ));
        assert!(same_visitor(&agent("a1", "p1"), &agent("a1", "p1")));
        // A signed-out person who now passes as anonymous on a public app.
        assert!(!same_visitor(&user("p1", &[]), &Visitor::Anonymous));
        assert!(!same_visitor(&Visitor::Anonymous, &user("p1", &[])));
        // A removed or added role, another person, another agent or another principal.
        assert!(!same_visitor(&user("p1", &["member"]), &user("p1", &[])));
        assert!(!same_visitor(&user("p1", &[]), &user("p1", &["member"])));
        assert!(!same_visitor(&user("p1", &[]), &user("p2", &[])));
        assert!(!same_visitor(&agent("a1", "p1"), &agent("a2", "p1")));
        assert!(!same_visitor(&agent("a1", "p1"), &agent("a1", "p2")));
        assert!(!same_visitor(&agent("a1", "p1"), &user("a1", &[])));
    }
}
