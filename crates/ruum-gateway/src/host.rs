//! Host names of app origins and the host lookup (`docs/STEP3.md` "Host names and slugs" and
//! "System lookups"; decision 0006-16).
//!
//! An app is `<app>.<tenant>.<apps domain>`, a preview `<short_id>--<app>.<tenant>.<apps
//! domain>`. [`parse`] reads only the labels: it never guesses a tenant from a name (decision
//! 0008-1: personal workspaces have random slugs). Every label follows the slug rules of
//! `apps::slugs` (reserved slugs, `--`, `xn--`, a `-` at an end, lengths); a short ID is 8
//! characters of `[a-z0-9]`. Host names are case-insensitive: the host is lower-cased, a port and
//! one trailing dot are dropped.
//!
//! [`HostLookup`] is the narrow system lookup: the SQL function `ruum_app_host_lookup` (migration
//! 0013, the gateway's database role may execute it and nothing else) returns only `(tenant_id,
//! app_id, deployment_id)`. [`HostCache`] keeps answers for at most [`HOST_TTL`]; a change of
//! the live deployment, of the settings or the trash invalidates it at once
//! ([`HostCache::invalidate_app`], fed by the journal events of W-1 and W-2s).

use async_trait::async_trait;
use ruum_core::apps::slugs::{self, SlugKind};
use ruum_core::{NodeId, TenantId};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long the cache keeps an answer of the host lookup (at most 5 s, card G-1a).
pub const HOST_TTL: Duration = Duration::from_secs(3);
/// The most host answers the cache keeps.
pub const HOST_CACHE_MAX: usize = 50_000;

/// The labels of an app host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppHost {
    pub tenant_slug: String,
    pub app_slug: String,
    /// The preview's short ID; `None` for the live app.
    pub short_id: Option<String>,
}

impl AppHost {
    /// The host name (lower case, no port).
    pub fn name(&self, apps_domain: &str) -> String {
        match &self.short_id {
            Some(s) => format!("{s}--{}.{}.{apps_domain}", self.app_slug, self.tenant_slug),
            None => format!("{}.{}.{apps_domain}", self.app_slug, self.tenant_slug),
        }
    }
}

/// The host of a request: lower case, without a port and one trailing dot. `None` for a value
/// that cannot be a host name.
pub fn normalize(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('[') {
        return None;
    }
    let host = match raw.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        Some(_) => return None,
        None => raw,
    };
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    if host.is_empty()
        || host.len() > 253
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return None;
    }
    Some(host)
}

fn short_id_ok(s: &str) -> bool {
    s.len() == 8
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Parses an app host of `apps_domain`. `None` for anything else: another domain, the apps domain
/// itself, a label count other than two in front of it, a label that breaks the slug rules (a
/// reserved slug, `--` in a slug, an `xn--` prefix, a `-` at an end, a wrong length), or a bad
/// short ID.
pub fn parse(raw_host: &str, apps_domain: &str) -> Option<AppHost> {
    let host = normalize(raw_host)?;
    let domain = apps_domain.trim_matches('.').to_ascii_lowercase();
    let front = host.strip_suffix(&domain)?.strip_suffix('.')?;
    let (first, tenant) = front.split_once('.')?;
    if tenant.contains('.') {
        return None;
    }
    if !slugs::is_valid(tenant, SlugKind::Tenant) {
        return None;
    }
    let (short_id, app) = match first.split_once("--") {
        Some((s, a)) => {
            if !short_id_ok(s) {
                return None;
            }
            (Some(s.to_string()), a)
        }
        None => (None, first),
    };
    if !slugs::is_valid(app, SlugKind::App) {
        return None;
    }
    Some(AppHost {
        tenant_slug: tenant.to_string(),
        app_slug: app.to_string(),
        short_id,
    })
}

/// What a host resolves to: the only values the system lookup returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub tenant_id: TenantId,
    pub app_id: NodeId,
    pub deployment_id: Uuid,
}

/// The narrow system lookup "host to app and deployment".
#[async_trait]
pub trait HostLookup: Send + Sync {
    /// The live deployment of the app (no short ID) or that ready preview; `None` for every
    /// other host.
    async fn lookup(&self, host: &AppHost) -> ruum_core::Result<Option<Resolved>>;
}

/// The lookup through the SQL function `ruum_app_host_lookup`, as the gateway's database role.
pub struct PgHostLookup {
    pool: sqlx::PgPool,
}

impl PgHostLookup {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl HostLookup for PgHostLookup {
    async fn lookup(&self, host: &AppHost) -> ruum_core::Result<Option<Resolved>> {
        let row: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "select tenant_id, app_id, deployment_id from ruum_app_host_lookup($1, $2, $3)",
        )
        .bind(&host.tenant_slug)
        .bind(&host.app_slug)
        .bind(host.short_id.as_deref())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(t, a, d)| Resolved {
            tenant_id: TenantId(t),
            app_id: NodeId(a),
            deployment_id: d,
        }))
    }
}

/// A lookup over a map (tests and the T-6 world, which seed hosts as the core's stores would).
#[derive(Default)]
pub struct MemoryHostLookup {
    hosts: Mutex<HashMap<AppHost, Resolved>>,
}

impl MemoryHostLookup {
    pub fn set(&self, host: AppHost, to: Resolved) {
        self.hosts.lock().expect("hosts lock").insert(host, to);
    }

    pub fn remove(&self, host: &AppHost) {
        self.hosts.lock().expect("hosts lock").remove(host);
    }

    /// Removes every host of `app`.
    pub fn remove_app(&self, app: NodeId) {
        self.hosts
            .lock()
            .expect("hosts lock")
            .retain(|_, r| r.app_id != app);
    }
}

#[async_trait]
impl HostLookup for MemoryHostLookup {
    async fn lookup(&self, host: &AppHost) -> ruum_core::Result<Option<Resolved>> {
        Ok(self.hosts.lock().expect("hosts lock").get(host).copied())
    }
}

/// The host lookup with a short cache (see the module docs).
pub struct HostCache {
    inner: std::sync::Arc<dyn HostLookup>,
    ttl: Duration,
    entries: Mutex<HashMap<AppHost, (Instant, Option<Resolved>)>>,
}

impl HostCache {
    pub fn new(inner: std::sync::Arc<dyn HostLookup>) -> Self {
        Self::with_ttl(inner, HOST_TTL)
    }

    pub fn with_ttl(inner: std::sync::Arc<dyn HostLookup>, ttl: Duration) -> Self {
        Self {
            inner,
            ttl: ttl.min(Duration::from_secs(5)),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub async fn lookup(&self, host: &AppHost) -> ruum_core::Result<Option<Resolved>> {
        {
            let e = self.entries.lock().expect("host cache lock");
            if let Some((at, r)) = e.get(host)
                && at.elapsed() < self.ttl
            {
                return Ok(*r);
            }
        }
        let r = self.inner.lookup(host).await?;
        let mut e = self.entries.lock().expect("host cache lock");
        if e.len() >= HOST_CACHE_MAX {
            let ttl = self.ttl;
            e.retain(|_, (at, _)| at.elapsed() < ttl);
            if e.len() >= HOST_CACHE_MAX {
                e.clear();
            }
        }
        e.insert(host.clone(), (Instant::now(), r));
        Ok(r)
    }

    /// Forgets every answer that names `app` (a publish, a rollback, a settings change, the
    /// trash: the journal events of the app).
    pub fn invalidate_app(&self, tenant: TenantId, app: NodeId) {
        self.entries
            .lock()
            .expect("host cache lock")
            .retain(|_, (_, r)| !r.is_some_and(|r| r.tenant_id == tenant && r.app_id == app));
    }

    /// Forgets every answer of `tenant_slug` (also "no such app" answers).
    pub fn invalidate_tenant_slug(&self, tenant_slug: &str) {
        self.entries
            .lock()
            .expect("host cache lock")
            .retain(|h, _| h.tenant_slug != tenant_slug);
    }

    /// Forgets every answer.
    pub fn invalidate_all(&self) {
        self.entries.lock().expect("host cache lock").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str = "ruumapps.localhost";

    fn live(app: &str, tenant: &str) -> Option<AppHost> {
        Some(AppHost {
            tenant_slug: tenant.into(),
            app_slug: app.into(),
            short_id: None,
        })
    }

    #[test]
    fn live_and_preview_hosts_parse() {
        assert_eq!(
            parse("garden.acme.ruumapps.localhost", D),
            live("garden", "acme")
        );
        assert_eq!(
            parse("k3m9q2xa--garden.acme.ruumapps.localhost", D),
            Some(AppHost {
                tenant_slug: "acme".into(),
                app_slug: "garden".into(),
                short_id: Some("k3m9q2xa".into()),
            })
        );
        // Random workspace slugs are plain labels (decision 0008-1).
        assert_eq!(
            parse("garden.ws-7k2m9q.ruumapps.localhost", D),
            live("garden", "ws-7k2m9q")
        );
    }

    #[test]
    fn case_port_and_trailing_dot_do_not_matter() {
        for h in [
            "GARDEN.Acme.RuumApps.Localhost",
            "garden.acme.ruumapps.localhost:443",
            "garden.acme.ruumapps.localhost.",
            "garden.acme.ruumapps.localhost.:8443",
        ] {
            assert_eq!(parse(h, D), live("garden", "acme"), "{h}");
        }
        assert_eq!(
            parse("garden.acme.ruumapps.localhost", ".RuumApps.localhost."),
            live("garden", "acme")
        );
    }

    #[test]
    fn other_hosts_are_refused() {
        for h in [
            "",
            "ruumapps.localhost",
            "acme.ruumapps.localhost",
            "x.garden.acme.ruumapps.localhost",
            "garden.acme.example.com",
            "garden.acme.ruumapps.localhost.example.com",
            "garden.acmeruumapps.localhost",
            "garden.acme.ruumapps.localhost:",
            "garden.acme.ruumapps.localhost:x",
            "[::1]:443",
            "gar_den.acme.ruumapps.localhost",
            "garden.acme.ruumapps.localhost..",
            "garden..ruumapps.localhost",
        ] {
            assert_eq!(parse(h, D), None, "{h}");
        }
    }

    #[test]
    fn slug_rules_apply_to_both_labels() {
        for h in [
            // reserved slugs, as tenant and as app
            "garden.content.ruumapps.localhost",
            "garden.www.ruumapps.localhost",
            "admin.acme.ruumapps.localhost",
            "api.acme.ruumapps.localhost",
            "mta-sts.acme.ruumapps.localhost",
            // shorter than 3
            "zi.acme.ruumapps.localhost",
            "garden.ac.ruumapps.localhost",
            // `--` in a slug, `xn--`, a dash at an end
            "zinnia--app.acme.ruumapps.localhost",
            "xn--garden.acme.ruumapps.localhost",
            "garden.xn--acme.ruumapps.localhost",
            "-garden.acme.ruumapps.localhost",
            "garden-.acme.ruumapps.localhost",
            "garden.-acme.ruumapps.localhost",
            // too long: app 54, tenant 41
            &format!("{}.acme.ruumapps.localhost", "a".repeat(54)),
            &format!("garden.{}.ruumapps.localhost", "a".repeat(41)),
        ] {
            assert_eq!(parse(h, D), None, "{h}");
        }
        assert!(parse(&format!("{}.acme.ruumapps.localhost", "a".repeat(53)), D).is_some());
        assert!(parse(&format!("garden.{}.ruumapps.localhost", "a".repeat(40)), D).is_some());
    }

    #[test]
    fn short_ids_are_eight_lower_case_characters() {
        for h in [
            "k3m9q2x--garden.acme.ruumapps.localhost",
            "k3m9q2xab--garden.acme.ruumapps.localhost",
            "k3m9-2xa--garden.acme.ruumapps.localhost",
            "--garden.acme.ruumapps.localhost",
            "k3m9q2xa--.acme.ruumapps.localhost",
            "k3m9q2xa--xn--garden.acme.ruumapps.localhost",
            "k3m9q2xa--gar--den.acme.ruumapps.localhost",
            "k3m9q2xa--admin.acme.ruumapps.localhost",
        ] {
            assert_eq!(parse(h, D), None, "{h}");
        }
        // An upper-case short ID is the same host (host names ignore case).
        assert_eq!(
            parse("K3M9Q2XA--garden.acme.ruumapps.localhost", D).and_then(|h| h.short_id),
            Some("k3m9q2xa".into())
        );
    }

    #[tokio::test]
    async fn the_cache_keeps_answers_briefly_and_forgets_on_invalidation() {
        let mem = std::sync::Arc::new(MemoryHostLookup::default());
        let cache = HostCache::with_ttl(mem.clone(), Duration::from_millis(200));
        let host = parse("garden.acme.ruumapps.localhost", D).unwrap();
        let r = Resolved {
            tenant_id: TenantId::new(),
            app_id: NodeId::new(),
            deployment_id: Uuid::now_v7(),
        };
        assert_eq!(cache.lookup(&host).await.unwrap(), None);
        mem.set(host.clone(), r);
        // The cached "no" holds for the TTL ...
        assert_eq!(cache.lookup(&host).await.unwrap(), None);
        cache.invalidate_tenant_slug("acme");
        assert_eq!(cache.lookup(&host).await.unwrap(), Some(r));
        mem.remove(&host);
        assert_eq!(cache.lookup(&host).await.unwrap(), Some(r));
        cache.invalidate_app(r.tenant_id, r.app_id);
        assert_eq!(cache.lookup(&host).await.unwrap(), None);
        mem.set(host.clone(), r);
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(cache.lookup(&host).await.unwrap(), Some(r));
        // The TTL is never above 5 s.
        let long = HostCache::with_ttl(mem, Duration::from_secs(60));
        assert!(long.ttl <= Duration::from_secs(5));
    }
}
