//! Configuration from the environment. Every setting has a `RUUM_` name, except `DATABASE_URL`
//! and `SMTP_URL`. Other crates read more settings (identity, SpiceDB, S3, KEK, WebDAV). Every
//! name the server reads is listed in `crates/ruum-server/env-names.txt`; the staging tests
//! (`deploy/staging/tests/server-env.sh`) check that list against the code and the Helm chart.

use anyhow::Context as _;
use std::path::PathBuf;

/// The default listen address. Ports 5433, 8080, 8787 and 8788 are taken by another local project.
pub const DEFAULT_BIND: &str = "127.0.0.1:8490";
/// The default listen address of the WebDAV host (decision 0002-3).
pub const DEFAULT_DAV_BIND: &str = "127.0.0.1:8492";

// The OIDC and S3 settings are read again by `ruum_core` (`identity::setup`, `blobs::object`);
// these copies are not used yet.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Config {
    /// `RUUM_BIND`
    pub bind: String,
    /// `RUUM_PUBLIC_URL`
    pub public_url: String,
    /// `RUUM_CONTENT_URL`: the user content domain (downloads, share links).
    pub content_url: String,
    /// `RUUM_DAV_BIND`: the listener of the WebDAV host (decision 0002-3).
    pub dav_bind: String,
    /// `RUUM_DAV_URL`: the public URL of the WebDAV host, for example
    /// `https://dav.ruum.space/`. Default: `https://dav.<host>/` for an https public URL, else
    /// `http://<RUUM_DAV_BIND>/`.
    pub dav_url: String,
    /// `DATABASE_URL`. Without it the server uses in-memory services, but only on a laptop
    /// (`RUUM_MIGRATE` on): with `RUUM_MIGRATE=false` it refuses to start (see `migrate::plan`).
    pub database_url: Option<String>,
    /// `RUUM_DB_MAX_CONNECTIONS`
    pub db_max_connections: u32,
    /// `RUUM_MIGRATE` (`true` or `false`): run migrations at start. A cluster turns it off and
    /// runs `ruum-server migrate` as its own step before the rollout (design 12.7); the server
    /// then only checks that the database has every migration of its release.
    pub migrate: bool,
    /// `RUUM_MIGRATIONS_DIR`
    pub migrations_dir: PathBuf,
    /// `RUUM_DB_RUNTIME_ROLE`: the database role of the serving pods. After the migrations,
    /// `ruum-server migrate` takes every write right on `_sqlx_migrations` from it and checks that
    /// it has no DDL rights (`migrate::restrict_runtime_role`). Set only on the migration Job.
    pub db_runtime_role: Option<String>,
    /// `RUUM_OIDC_ISSUER`: the Zitadel issuer URL.
    pub oidc_issuer: Option<String>,
    /// `RUUM_OIDC_CLIENT_ID`
    pub oidc_client_id: Option<String>,
    /// `RUUM_OIDC_CLIENT_SECRET`
    pub oidc_client_secret: Option<String>,
    /// `RUUM_S3_ENDPOINT`: Garage on the laptop, Hetzner Object Storage in production.
    pub s3_endpoint: Option<String>,
    /// `RUUM_S3_BUCKET`
    pub s3_bucket: Option<String>,
    /// `RUUM_DBHOST_URL`: the `ruum_dbhost` URL of the data cluster (card D-1). Without it the
    /// server has no database engine and `createDatabase` answers `422` (no node is made).
    /// With it the server needs a key-encryption key (`RUUM_LOCAL_KEK_FILE` or
    /// `RUUM_OPENBAO_ADDR`) or it does not start. `wiring::install_databases` reads it.
    pub dbhost_url: Option<String>,
    /// `RUUM_REQUEST_TIMEOUT_SECONDS`: a request without an answer after this time gets `408`
    /// (default 120; the longest long poll is 30 seconds).
    pub request_timeout: std::time::Duration,
    /// `RUUM_INTERNAL_BIND`: the listener of the internal router (`ruum_api::internal`: the
    /// on-demand TLS `ask` endpoint of the edge). Off when not set. Never route it through the
    /// public edge.
    pub internal_bind: Option<String>,
    /// `RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR`: new content hosts that may get a certificate per hour
    /// (default 20).
    pub tls_ask_new_hosts_per_hour: usize,
    /// `RUUM_SHUTDOWN_GRACE_SECONDS`: after SIGTERM, how long open requests may finish
    /// (default 35, longer than the longest long poll; `shutdown.rs`).
    pub shutdown_grace: std::time::Duration,
    /// `RUUM_SIGNUP_ALLOWED_EMAILS` and `RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS`: the allow-list of
    /// a closed server (`ruum_api::signup`). `None` when neither is set: open.
    pub signup: Option<ruum_api::signup::EmailAllowList>,
    /// The telemetry role (card O-1b): `RUUM_TELEMETRY` and the `RUUM_TELEMETRY_*` settings.
    pub telemetry: crate::telemetry::TelemetrySettings,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let public_url = var("RUUM_PUBLIC_URL").unwrap_or_else(|| "https://ruum.localhost".into());
        let dav_bind = var("RUUM_DAV_BIND").unwrap_or_else(|| DEFAULT_DAV_BIND.into());
        let dav_url = var("RUUM_DAV_URL")
            .map(|u| format!("{}/", u.trim_end_matches('/')))
            .unwrap_or_else(|| default_dav_url(&public_url, &dav_bind));
        Ok(Self {
            bind: var("RUUM_BIND").unwrap_or_else(|| DEFAULT_BIND.into()),
            public_url,
            content_url: var("RUUM_CONTENT_URL")
                .unwrap_or_else(|| "https://content.ruumapps.localhost".into()),
            dav_bind,
            dav_url,
            database_url: var("DATABASE_URL"),
            db_max_connections: var("RUUM_DB_MAX_CONNECTIONS")
                .map(|v| v.parse())
                .transpose()
                .context("RUUM_DB_MAX_CONNECTIONS must be a number")?
                .unwrap_or(10),
            migrate: var("RUUM_MIGRATE")
                .map(|v| v.parse())
                .transpose()
                .context("RUUM_MIGRATE must be true or false")?
                .unwrap_or(true),
            migrations_dir: var("RUUM_MIGRATIONS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("migrations")),
            db_runtime_role: var("RUUM_DB_RUNTIME_ROLE"),
            oidc_issuer: var("RUUM_OIDC_ISSUER"),
            oidc_client_id: var("RUUM_OIDC_CLIENT_ID"),
            oidc_client_secret: var("RUUM_OIDC_CLIENT_SECRET"),
            s3_endpoint: var("RUUM_S3_ENDPOINT"),
            s3_bucket: var("RUUM_S3_BUCKET"),
            dbhost_url: var("RUUM_DBHOST_URL"),
            request_timeout: std::time::Duration::from_secs(
                var("RUUM_REQUEST_TIMEOUT_SECONDS")
                    .map(|v| v.parse::<u64>())
                    .transpose()
                    .context("RUUM_REQUEST_TIMEOUT_SECONDS must be a number")?
                    .unwrap_or(120)
                    .max(31),
            ),
            internal_bind: var("RUUM_INTERNAL_BIND"),
            tls_ask_new_hosts_per_hour: var("RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR")
                .map(|v| v.parse::<usize>())
                .transpose()
                .context("RUUM_TLS_ASK_NEW_HOSTS_PER_HOUR must be a number")?
                .unwrap_or(20),
            shutdown_grace: var("RUUM_SHUTDOWN_GRACE_SECONDS")
                .map(|v| v.parse::<u64>())
                .transpose()
                .context("RUUM_SHUTDOWN_GRACE_SECONDS must be a number")?
                .map(std::time::Duration::from_secs)
                .unwrap_or(crate::shutdown::DEFAULT_GRACE),
            signup: ruum_api::signup::EmailAllowList::from_settings(
                var("RUUM_SIGNUP_ALLOWED_EMAILS").as_deref(),
                var("RUUM_SIGNUP_ALLOWED_UNVERIFIED_EMAILS").as_deref(),
            ),
            telemetry: telemetry_settings()?,
        })
    }
}

/// The workload token settings (card R-3): `RUUM_WORKLOAD_TOKEN_KEY_FILE`, the Ed25519 seed of
/// the workload token issuer (a file, never an environment value), and
/// `RUUM_WORKLOAD_JWKS_FILE`, where the server writes the public key set at start for the
/// runtime API (`RUUM_RUNTIME_WORKLOAD_JWKS_FILE` there). Both optional: without a key the
/// server issues no workload tokens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkloadTokenSettings {
    pub key_file: Option<PathBuf>,
    pub jwks_file: Option<PathBuf>,
}

impl WorkloadTokenSettings {
    pub fn from_env() -> Self {
        Self {
            key_file: var("RUUM_WORKLOAD_TOKEN_KEY_FILE").map(PathBuf::from),
            jwks_file: var("RUUM_WORKLOAD_JWKS_FILE").map(PathBuf::from),
        }
    }
}

/// The runtime driver of the sandboxes (backlog I6-1). `RUUM_RUNTIME_DRIVER`: `memory` (the
/// default: nothing runs) or `kubernetes` (pods in gVisor; the Docker driver of the laptop is
/// wired by card I-4). The Kubernetes settings:
///
/// - `RUUM_KUBE_API_URL` (default `https://kubernetes.default.svc`; `https://` only),
///   `RUUM_KUBE_TOKEN_FILE` and `RUUM_KUBE_CA_FILE` (the projected token and the cluster CA);
/// - `RUUM_KUBE_NAMESPACE` (default `ruum-apps`) and `RUUM_KUBE_RUNTIME_CLASS` (default
///   `gvisor`);
/// - `RUUM_REGISTRY`: the registry the nodes pull app images from (`host:port`); without it
///   every app image is `image_not_found` (fails closed);
/// - `RUUM_KUBE_EGRESS_PROXY`: the egress proxy's Service as `host:port` (the sandboxes' only
///   way out). `RUUM_KUBE_EGRESS_CONTROL` and `RUUM_KUBE_EGRESS_CONTROL_TOKEN_FILE` (the
///   control listener, where the driver would register each pod's address, backlog I6-2) are
///   refused until backlog S-86 is fixed (a registration can outlive its pod without a record,
///   and after an address is reused the proxy names the wrong app; Kimi run 15, I6-1 finding
///   3). Without them no pod is registered and the proxy refuses every sandbox (fails closed);
/// - `RUUM_KUBE_SPLIT_LOG_STREAMS` (`on` or `off`, default `off`): read standard output and
///   standard error apart (the API server's feature gate `PodLogsQuerySplitStreams`).
/// - The sandbox supervisor (backlog I6-20, S-89; `docs/STEP3.md` "The sandbox supervisor"), all
///   required with `kubernetes`: `RUUM_KUBE_RECORD_KEY_FILE` (64 hexadecimal characters: the
///   key that seals the values in the instance records), `RUUM_KUBE_SUPERVISOR_IMAGE` (the
///   supervisor's image, by digest), `RUUM_KUBE_SUPERVISOR_SERVICE` (`host:port`: the listener as
///   the sandboxes reach it, a Service), `RUUM_KUBE_SUPERVISOR_LISTEN` (`addr:port`: the
///   listener of this process) and `RUUM_KUBE_SUPERVISOR_NETWORK=encrypted` (node-to-node
///   traffic is encrypted and authenticated: the launch channel is plain HTTP; decision 0006 I
///   point 4). `RUUM_KUBE_NODE_ARCH` (default `amd64`): the platform of a multi-arch image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeSettings {
    Memory,
    Kubernetes(Box<KubeSettings>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeSettings {
    pub api_url: String,
    pub token_file: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
    pub namespace: String,
    pub runtime_class: String,
    pub registry: Option<String>,
    pub egress_proxy: Option<(String, u16)>,
    pub egress_control: Option<String>,
    pub egress_control_token_file: Option<PathBuf>,
    pub split_log_streams: bool,
    pub record_key_file: PathBuf,
    pub supervisor_image: String,
    pub supervisor_service: (String, u16),
    pub supervisor_listen: std::net::SocketAddr,
    pub node_arch: String,
}

/// `host:port` of a setting.
fn host_port(name: &str, v: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) = v
        .rsplit_once(':')
        .with_context(|| format!("{name} must be host:port"))?;
    let port: u16 = port
        .parse()
        .with_context(|| format!("{name} must be host:port"))?;
    anyhow::ensure!(!host.is_empty() && port != 0, "{name} must be host:port");
    Ok((host.to_owned(), port))
}

impl RuntimeSettings {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(var)
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        match get("RUUM_RUNTIME_DRIVER").as_deref() {
            None | Some("memory") => return Ok(Self::Memory),
            Some("kubernetes") => {}
            Some(other) => anyhow::bail!(
                "RUUM_RUNTIME_DRIVER must be memory or kubernetes, not {other:?} (the Docker driver is wired by card I-4)"
            ),
        }
        let egress_proxy = match get("RUUM_KUBE_EGRESS_PROXY") {
            None => None,
            Some(v) => {
                let (host, port) = v
                    .rsplit_once(':')
                    .context("RUUM_KUBE_EGRESS_PROXY must be host:port")?;
                let port: u16 = port
                    .parse()
                    .context("RUUM_KUBE_EGRESS_PROXY must be host:port")?;
                anyhow::ensure!(
                    !host.is_empty() && port != 0,
                    "RUUM_KUBE_EGRESS_PROXY must be host:port"
                );
                Some((host.to_owned(), port))
            }
        };
        let egress_control = get("RUUM_KUBE_EGRESS_CONTROL");
        // The control token never crosses a network in clear: a Unix socket only, until the
        // listener has authenticated TLS (backlog I6-2).
        if let Some(c) = &egress_control {
            anyhow::ensure!(
                c.starts_with("unix:/"),
                "RUUM_KUBE_EGRESS_CONTROL must be a Unix socket (unix:/path)"
            );
        }
        let egress_control_token_file =
            get("RUUM_KUBE_EGRESS_CONTROL_TOKEN_FILE").map(PathBuf::from);
        anyhow::ensure!(
            egress_control.is_some() == egress_control_token_file.is_some(),
            "RUUM_KUBE_EGRESS_CONTROL and RUUM_KUBE_EGRESS_CONTROL_TOKEN_FILE are set together"
        );
        // Not before S-86 is fixed (Kimi run 15, I6-1 finding 3): a registration that outlives
        // its pod with no record names the wrong app once the address is reused.
        anyhow::ensure!(
            egress_control.is_none(),
            "RUUM_KUBE_EGRESS_CONTROL is refused until backlog S-86 is fixed (an egress registration can outlive its pod); leave it unset, the proxy then refuses every sandbox"
        );
        let split_log_streams = match get("RUUM_KUBE_SPLIT_LOG_STREAMS").as_deref() {
            None | Some("off") => false,
            Some("on") => true,
            Some(other) => {
                anyhow::bail!("RUUM_KUBE_SPLIT_LOG_STREAMS must be on or off, not {other:?}")
            }
        };
        // The sandbox supervisor (I6-20, S-89): no Kubernetes driver without it.
        let need = |name: &str| {
            get(name).with_context(|| {
                format!("{name} is required with RUUM_RUNTIME_DRIVER=kubernetes (the sandbox supervisor, backlog S-89)")
            })
        };
        let record_key_file = PathBuf::from(need("RUUM_KUBE_RECORD_KEY_FILE")?);
        let supervisor_image = need("RUUM_KUBE_SUPERVISOR_IMAGE")?;
        anyhow::ensure!(
            supervisor_image.contains("@sha256:"),
            "RUUM_KUBE_SUPERVISOR_IMAGE must name the image by digest (name@sha256:...)"
        );
        let supervisor_service = host_port(
            "RUUM_KUBE_SUPERVISOR_SERVICE",
            &need("RUUM_KUBE_SUPERVISOR_SERVICE")?,
        )?;
        let supervisor_listen: std::net::SocketAddr = need("RUUM_KUBE_SUPERVISOR_LISTEN")?
            .parse()
            .context("RUUM_KUBE_SUPERVISOR_LISTEN must be addr:port")?;
        anyhow::ensure!(
            get("RUUM_KUBE_SUPERVISOR_NETWORK").as_deref() == Some("encrypted"),
            "RUUM_KUBE_SUPERVISOR_NETWORK must be encrypted: the launch channel is plain HTTP, so node-to-node traffic must be encrypted and authenticated first (KubeSpan or the CNI's WireGuard; decision 0006 I point 4)"
        );
        // The service account token goes to the API server over TLS only.
        let api_url =
            get("RUUM_KUBE_API_URL").unwrap_or_else(|| "https://kubernetes.default.svc".into());
        anyhow::ensure!(
            api_url.starts_with("https://"),
            "RUUM_KUBE_API_URL must be an https:// address"
        );
        Ok(Self::Kubernetes(Box::new(KubeSettings {
            api_url,
            token_file: get("RUUM_KUBE_TOKEN_FILE").map(PathBuf::from),
            ca_file: get("RUUM_KUBE_CA_FILE").map(PathBuf::from),
            namespace: get("RUUM_KUBE_NAMESPACE").unwrap_or_else(|| "ruum-apps".into()),
            runtime_class: get("RUUM_KUBE_RUNTIME_CLASS").unwrap_or_else(|| "gvisor".into()),
            registry: get("RUUM_REGISTRY"),
            egress_proxy,
            egress_control,
            egress_control_token_file,
            split_log_streams,
            record_key_file,
            supervisor_image,
            supervisor_service,
            supervisor_listen,
            node_arch: get("RUUM_KUBE_NODE_ARCH").unwrap_or_else(|| "amd64".into()),
        })))
    }
}

/// The settings of the telemetry role (card O-1b). Each has a safe default; the role starts in
/// the serving process only with `RUUM_TELEMETRY=on`.
fn telemetry_settings() -> anyhow::Result<crate::telemetry::TelemetrySettings> {
    let d = crate::telemetry::TelemetrySettings::default();
    Ok(crate::telemetry::TelemetrySettings {
        enabled: crate::telemetry::parse_switch(var("RUUM_TELEMETRY").as_deref())?,
        grpc_listen: var("RUUM_TELEMETRY_GRPC_LISTEN").unwrap_or(d.grpc_listen),
        http_listen: var("RUUM_TELEMETRY_HTTP_LISTEN").unwrap_or(d.http_listen),
        collector_url: var("RUUM_TELEMETRY_COLLECTOR_URL").unwrap_or(d.collector_url),
        collector_token_file: var("RUUM_TELEMETRY_COLLECTOR_TOKEN_FILE").map(PathBuf::from),
        loki_url: var("RUUM_LOKI_URL").unwrap_or(d.loki_url),
        mimir_url: var("RUUM_MIMIR_URL").unwrap_or(d.mimir_url),
        tempo_url: var("RUUM_TEMPO_URL").unwrap_or(d.tempo_url),
        shipper_state: var("RUUM_TELEMETRY_SHIPPER_STATE").map(PathBuf::from),
    })
}

/// The URL of the WebDAV host when `RUUM_DAV_URL` is not set: `https://dav.<host>/` next to an
/// https public URL (Caddy on the laptop, the load balancer in production), else the listener.
fn default_dav_url(public_url: &str, dav_bind: &str) -> String {
    match public_url.strip_prefix("https://") {
        Some(host) => format!("https://dav.{}/", host.trim_end_matches('/')),
        None => format!("http://{dav_bind}/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dav_url_follows_the_public_url() {
        assert_eq!(
            default_dav_url("https://ruum.localhost", DEFAULT_DAV_BIND),
            "https://dav.ruum.localhost/"
        );
        assert_eq!(
            default_dav_url("https://ruum.space/", DEFAULT_DAV_BIND),
            "https://dav.ruum.space/"
        );
        assert_eq!(
            default_dav_url("http://127.0.0.1:8490", "127.0.0.1:8492"),
            "http://127.0.0.1:8492/"
        );
    }

    fn runtime(pairs: &[(&str, &str)]) -> anyhow::Result<RuntimeSettings> {
        let m: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        RuntimeSettings::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn the_runtime_driver_is_memory_unless_kubernetes_is_asked() {
        assert_eq!(runtime(&[]).unwrap(), RuntimeSettings::Memory);
        assert!(runtime(&[("RUUM_RUNTIME_DRIVER", "docker")]).is_err());
        let digest = format!("r/ruum-supervisor@sha256:{}", "a".repeat(64));
        let base = [
            ("RUUM_RUNTIME_DRIVER", "kubernetes"),
            ("RUUM_KUBE_RECORD_KEY_FILE", "/run/ruum-runtime/record-key"),
            ("RUUM_KUBE_SUPERVISOR_IMAGE", digest.as_str()),
            (
                "RUUM_KUBE_SUPERVISOR_SERVICE",
                "ruum-supervisor.ruum.svc.cluster.local:8091",
            ),
            ("RUUM_KUBE_SUPERVISOR_LISTEN", "0.0.0.0:8091"),
            ("RUUM_KUBE_SUPERVISOR_NETWORK", "encrypted"),
        ];
        let mut ok = base.to_vec();
        ok.push((
            "RUUM_KUBE_EGRESS_PROXY",
            "ruum-egress.ruum.svc.cluster.local:3128",
        ));
        let RuntimeSettings::Kubernetes(k) = runtime(&ok).unwrap() else {
            panic!("not kubernetes");
        };
        assert_eq!(k.namespace, "ruum-apps");
        assert_eq!(k.runtime_class, "gvisor");
        assert_eq!(k.registry, None);
        assert_eq!(
            k.egress_proxy,
            Some(("ruum-egress.ruum.svc.cluster.local".to_owned(), 3128))
        );
        assert!(!k.split_log_streams);
        assert_eq!(k.supervisor_listen.port(), 8091);
        assert_eq!(k.node_arch, "amd64");
        // The supervisor settings are required, the image by digest, the network encrypted.
        for missing in base.iter().skip(1) {
            let all: Vec<_> = base.iter().filter(|x| x != &missing).cloned().collect();
            assert!(runtime(&all).is_err(), "without {}", missing.0);
        }
        for (name, value) in [
            ("RUUM_KUBE_SUPERVISOR_IMAGE", "r/ruum-supervisor:latest"),
            ("RUUM_KUBE_SUPERVISOR_NETWORK", "plain"),
            ("RUUM_KUBE_SUPERVISOR_SERVICE", "no-port"),
            ("RUUM_KUBE_SUPERVISOR_LISTEN", "nowhere"),
        ] {
            let mut all: Vec<_> = base.iter().filter(|x| x.0 != name).cloned().collect();
            all.push((name, value));
            assert!(runtime(&all).is_err(), "{name}={value}");
        }
        for bad in [
            &[("RUUM_KUBE_EGRESS_PROXY", "no-port")][..],
            &[("RUUM_KUBE_EGRESS_CONTROL", "unix:/run/x.sock")][..],
            &[("RUUM_KUBE_SPLIT_LOG_STREAMS", "yes")][..],
            // Credentials never in clear (Kimi review of I6-1, finding 9).
            &[("RUUM_KUBE_API_URL", "http://kubernetes.default.svc")][..],
            &[
                ("RUUM_KUBE_EGRESS_CONTROL", "http://ruum-egress:3129"),
                ("RUUM_KUBE_EGRESS_CONTROL_TOKEN_FILE", "/run/t"),
            ][..],
            // Not before S-86 is fixed (Kimi run 15, I6-1 finding 3), even on a Unix socket.
            &[
                (
                    "RUUM_KUBE_EGRESS_CONTROL",
                    "unix:/run/ruum-egress/control.sock",
                ),
                ("RUUM_KUBE_EGRESS_CONTROL_TOKEN_FILE", "/run/t"),
            ][..],
        ] {
            let mut all = base.to_vec();
            all.extend_from_slice(bad);
            assert!(runtime(&all).is_err(), "{bad:?}");
        }
    }
}
