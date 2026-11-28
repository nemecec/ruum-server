//! The `node22` builder on Kubernetes (card I6-13; backlog I6-13, P-84): [`KubeBuilder`]
//! implements [`ruum_core::runtime::Builder`] with the fixed recipe of the Docker builder
//! (card R-2b, the model) as a **gVisor pod**, on clusters without a Docker engine (Talos).
//!
//! A build, in order:
//!
//! 1. the input is checked, the runtime must be `node22`, and the build waits for a slot (the
//!    same [`Slots`] as the Docker builder: a global cap and a per-tenant share);
//! 2. the archive is unpacked and checked by W-2s's reader and `app.yaml` is read
//!    ([`super::prepare`]): the recipe and the source tar;
//! 3. **the build pod** (`ruum-b-<build>` in the builds namespace, [`pod_body`]) runs the base
//!    image `node22` by digest from the cluster registry with the hardening of a sandbox:
//!    runtime class `gvisor`, a service account with no rights and no token, user 10001,
//!    read-only root, `drop: [ALL]`, no privilege escalation, `RuntimeDefault` seccomp, CPU and
//!    memory limits (requests = limits), two size-limited `emptyDir` folders (`/work`, `/tmp`),
//!    no DNS (an `/etc/hosts` line for the egress proxy), `restartPolicy: Never` and
//!    `activeDeadlineSeconds` of the build's timeout. Its only routes are the egress proxy (the
//!    npm registry) and the **build channel** of this process (the network policies of the
//!    namespace allow nothing else);
//! 4. the pod's fixed program ([`BUILD_SCRIPT`], `node -e`) fetches the source tar from the
//!    build channel (`GET /v1/builds/<build>/source`), runs `npm ci` with the install scripts
//!    and the manifest's `build` command (from `RUUM_BUILD_COMMAND`, never part of a script
//!    text), and streams the output tar back (`PUT /v1/builds/<build>/output`). The channel is
//!    TLS 1.3 with the certificate of the supervisor listener (derived from the record key,
//!    backlog I620-2), which the program pins (`RUUM_BUILD_PIN`); each request carries the
//!    build's own token (256 random bits, only in this process and the pod's environment), and
//!    the channel takes requests only from the build pod's address. The source is given once
//!    and the output taken once, under the cap
//!    ([`KubeBuilderConfig::output_max`]): a longer output fails the build;
//! 5. the output tar is assembled and pushed exactly as by the Docker builder
//!    ([`super::assemble`]): to `ruum/<tenant>/<app>` only, the manifest by digest, after the
//!    timeout with a short bound of its own (the commit point);
//! 6. the pod is deleted (its log read first, a bounded tail), its egress registration removed
//!    and the channel entry dropped after every outcome, also when the caller drops the build (a
//!    drop guard spawns the cleanup); the build time goes to the usage sink.
//!
//! **No secret reaches a build**: the pod's environment is a fixed list ([`pod_body`]): the
//! npm settings, the proxy, the build channel's address, pin, certificate (public) and the
//! build's token, which opens only this build's source and output. The registry credential and
//! the egress control token live only in this process. The pod mounts no Secret and no service
//! account token.
//!
//! The rights it needs (`deploy/staging/charts/platform/templates/builder-rbac.yaml`): pods
//! (create, get, list, delete) and `pods/log` (get) in the builds namespace only.

use super::{
    Assembly, NODE22_BASE_INDEX, Ready, Slot, Slots, assemble, failed, job, oci, prepare, registry,
    tail_text,
};
use crate::egress::{EgressNetwork, EgressRegistry};
use crate::kube::SharedService;
use crate::kube::api::{Api, Kind, Write};
use async_trait::async_trait;
use bytes::Bytes;
use ruum_core::runtime::builder::MAX_BUILD_LOG_LINES;
use ruum_core::runtime::{
    BUILD_ROUTES, BuildInput, BuildLogLine, BuildOutput, Builder, ImageRef, OciDigest,
    RuntimeError, RuntimeUsage, RuntimeUsageSink, WorkloadIdentity,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The namespace of the build pods.
pub const NAMESPACE: &str = "ruum-builds";
/// The service account of the build pods (no rights, no token mounted).
pub const SERVICE_ACCOUNT: &str = "ruum-build";
/// The container of a build pod.
pub const CONTAINER: &str = "build";
/// The user and group a build runs as.
pub const BUILD_UID: u32 = 10001;
/// The label of everything a build owns: its ID.
pub const LABEL_BUILD: &str = super::LABEL_BUILD;
/// The default output cap of a Kubernetes build (64 MiB).
pub const KUBE_OUTPUT_MAX: u64 = 64 * 1024 * 1024;

/// The reason of a build that found no free build slot in time.
pub const BUILD_SLOTS_BUSY: &str = "every build slot is busy; try again in a few minutes";

/// How long a build waits for a build slot ([`KubeBuilderConfig::slot_wait`]), then fails as
/// unavailable with "try again" (review I6-13, 1; backlog P-84).
pub const KUBE_SLOT_WAIT: Duration = Duration::from_secs(120);
/// The most bytes of a token or manifest answer of Docker Hub while mirroring the base.
const MIRROR_SMALL_MAX: usize = 1024 * 1024;
/// How often the builder reads the pod while the build runs.
const POLL: Duration = Duration::from_millis(1000);
/// The most bytes of the build log the builder reads back.
const LOG_BYTES: u64 = 256 * 1024;
/// How long a build waits after its pod ended for the channel to finish the output.
const OUTPUT_GRACE: Duration = Duration::from_secs(10);

/// The fixed program of a build pod (`node -e`). Nothing of the app is part of this text: the
/// optional `build` command reaches it as `RUUM_BUILD_COMMAND`. It removes the channel's
/// values from the environment before it starts npm, its install scripts and the build
/// command, so they inherit none of them (a script can still read the program's own initial
/// environment, which opens only this build's source and output). npm's output goes to standard
/// error; the output leaves only through the channel.
pub const BUILD_SCRIPT: &str = r#"'use strict';
const https = require('https');
const crypto = require('crypto');
const { spawn } = require('child_process');
const base = process.env.RUUM_BUILD_CHANNEL;
const token = process.env.RUUM_BUILD_TOKEN;
const pin = process.env.RUUM_BUILD_PIN;
const ca = process.env.RUUM_BUILD_CA;
const command = process.env.RUUM_BUILD_COMMAND || '';
for (const k of ['RUUM_BUILD_CHANNEL', 'RUUM_BUILD_TOKEN', 'RUUM_BUILD_PIN', 'RUUM_BUILD_CA', 'RUUM_BUILD_COMMAND']) delete process.env[k];
const log = (m) => process.stderr.write('ruum-build: ' + m + '\n');
function opts(method, path, headers) {
  const u = new URL(base + path);
  return {
    method, hostname: u.hostname, port: u.port, path: u.pathname, ca, agent: false,
    minVersion: 'TLSv1.3',
    checkServerIdentity: (_host, cert) =>
      ('sha256:' + crypto.createHash('sha256').update(cert.raw).digest('hex')) === pin
        ? undefined : new Error('the build channel certificate does not match its pin'),
    headers: Object.assign({ authorization: 'Bearer ' + token }, headers || {}),
  };
}
function run(cmd, args) {
  return new Promise((resolve) => {
    const p = spawn(cmd, args, { stdio: ['ignore', 2, 2] });
    p.on('error', () => resolve(127));
    p.on('close', (code, signal) => resolve(code === null ? 128 : code));
  });
}
function source() {
  return new Promise((resolve, reject) => {
    const req = https.request(opts('GET', '/source'), (res) => {
      if (res.statusCode !== 200) { res.resume(); reject(new Error('the source: HTTP ' + res.statusCode)); return; }
      const tar = spawn('tar', ['-x', '-f', '-', '-C', '/work/app'], { stdio: ['pipe', 2, 2] });
      res.pipe(tar.stdin);
      tar.on('close', (code) => code === 0 ? resolve() : reject(new Error('unpacking the source: tar ' + code)));
    });
    req.on('error', reject);
    req.end();
  });
}
function output() {
  return new Promise((resolve, reject) => {
    const tar = spawn('tar', ['-c', '-f', '-', '-C', '/work/app', '.'], { stdio: ['ignore', 'pipe', 2] });
    const req = https.request(opts('PUT', '/output', { 'content-type': 'application/x-tar' }), (res) => {
      res.resume();
      res.on('end', () => res.statusCode === 204 ? resolve() : reject(new Error('the output: HTTP ' + res.statusCode)));
    });
    req.on('error', reject);
    tar.on('close', (code) => { if (code !== 0) { req.destroy(); reject(new Error('packing the output: tar ' + code)); } });
    tar.stdout.pipe(req);
  });
}
(async () => {
  require('fs').mkdirSync('/work/app');
  await source();
  process.chdir('/work/app');
  log('npm ci');
  let code = await run('npm', ['ci', '--ignore-scripts=false', '--foreground-scripts', '--no-audit', '--no-fund']);
  if (code !== 0) { log('npm ci exited with code ' + code); process.exit(code); }
  if (command) {
    log('the build command');
    code = await run('sh', ['-c', command]);
    if (code !== 0) { log('the build command exited with code ' + code); process.exit(code); }
  }
  log('output');
  await output();
  process.exit(0);
})().catch((e) => { log(e.message); process.exit(1); });
"#;

/// The channel's TLS identity as the build pods pin it: the supervisor listener's certificate
/// (`crate::kube::tls::SupervisorTls`), public.
#[derive(Clone, Debug)]
pub struct ChannelTls {
    /// The certificate (DER).
    pub cert_der: Vec<u8>,
    /// `sha256:<hex>` of the certificate (`crate::kube::tls::pin_of`).
    pub pin: String,
}

impl ChannelTls {
    /// The certificate as PEM (the pod's `RUUM_BUILD_CA`).
    pub fn pem(&self) -> String {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&self.cert_der);
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).unwrap_or_default());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        pem
    }
}

/// The settings of [`KubeBuilder`]. The crate reads no environment: the server's wiring builds
/// it.
#[derive(Clone, Debug)]
pub struct KubeBuilderConfig {
    /// The API server (`https://kubernetes.default.svc`).
    pub api_url: String,
    /// The bearer token file (a projected service account token), read for every request.
    pub token_file: Option<PathBuf>,
    pub ca_file: Option<PathBuf>,
    pub api_timeout: Duration,
    /// The namespace of the build pods ([`NAMESPACE`]).
    pub namespace: String,
    pub runtime_class: String,
    pub service_account: String,
    /// The registry as the nodes pull from it (the build pod's image).
    pub node_registry: String,
    /// The registry as this process pushes to it (`http://host:port`).
    pub registry_url: String,
    pub base_repository: String,
    pub base_index: String,
    /// The nodes' architecture (`amd64`), which picks the base image of the index.
    pub node_arch: String,
    pub output_max: u64,
    pub max_parallel: usize,
    pub per_tenant: usize,
    /// How long a build waits for a slot ([`KUBE_SLOT_WAIT`]); then it fails with
    /// [`RuntimeError::Unavailable`] and its deployment frees its deploy slot.
    pub slot_wait: Duration,
    pub npm_registry: String,
    /// The allow list of a build at the egress proxy: the npm registry hosts.
    pub npm_hosts: Vec<String>,
    /// The egress proxy as the build pod reaches it (an `/etc/hosts` line); `None`: the build
    /// has no route to the internet (only builds without downloads work).
    pub egress_proxy: Option<SharedService>,
    /// The build channel as the build pods reach it: an IP address or a name this process
    /// resolves, and the port.
    pub channel_address: String,
    pub channel_port: u16,
    pub channel_tls: ChannelTls,
    pub credential: Option<registry::RegistryCredential>,
    pub token_realm: Option<String>,
    pub extra_labels: BTreeMap<String, String>,
    pub node_selector: Option<BTreeMap<String, String>>,
}

impl KubeBuilderConfig {
    /// The defaults of a cluster: the in-cluster API server, the builds namespace, gVisor.
    pub fn new(
        api_url: &str,
        node_registry: &str,
        registry_url: &str,
        channel_address: &str,
        channel_port: u16,
        channel_tls: ChannelTls,
    ) -> Self {
        Self {
            api_url: api_url.to_owned(),
            token_file: None,
            ca_file: None,
            api_timeout: Duration::from_secs(20),
            namespace: NAMESPACE.to_owned(),
            runtime_class: "gvisor".to_owned(),
            service_account: SERVICE_ACCOUNT.to_owned(),
            node_registry: node_registry.to_owned(),
            registry_url: registry_url.to_owned(),
            base_repository: super::BASE_REPOSITORY.to_owned(),
            base_index: NODE22_BASE_INDEX.to_owned(),
            node_arch: "amd64".to_owned(),
            // An output and its layer are in the server's memory until the push (Kimi I6-13, 3):
            // one build of at most 64 MiB at a time unless the settings say more.
            output_max: KUBE_OUTPUT_MAX,
            max_parallel: 1,
            per_tenant: 1,
            slot_wait: KUBE_SLOT_WAIT,
            npm_registry: super::NPM_REGISTRY.to_owned(),
            npm_hosts: vec!["registry.npmjs.org".to_owned()],
            egress_proxy: None,
            channel_address: channel_address.to_owned(),
            channel_port,
            channel_tls,
            credential: None,
            token_realm: None,
            extra_labels: BTreeMap::new(),
            node_selector: None,
        }
    }

    fn check(&self) -> Result<(), RuntimeError> {
        let bad = |reason: &str| {
            Err(RuntimeError::Internal {
                reason: reason.to_owned(),
            })
        };
        if self.max_parallel == 0 || self.per_tenant == 0 || self.per_tenant > self.max_parallel {
            return bad("the build slots: 0 < per_tenant <= max_parallel");
        }
        if self.slot_wait.is_zero() {
            return bad("the wait for a build slot must be above zero");
        }
        if self.output_max == 0 {
            return bad("the output cap must be above zero");
        }
        if self.node_registry.is_empty() || self.node_registry.contains('/') {
            return bad(
                "the node registry is a host and port, such as registry.ruum.internal:5000",
            );
        }
        if !self.registry_url.starts_with("http://") && !self.registry_url.starts_with("https://") {
            return bad("the registry URL must start with http:// or https://");
        }
        if self.channel_address.is_empty() || self.channel_port == 0 {
            return bad("the builder needs the build channel's address and port");
        }
        if !self.channel_tls.pin.starts_with("sha256:") || self.channel_tls.cert_der.is_empty() {
            return bad("the build channel needs its TLS certificate and pin");
        }
        if self.npm_hosts.is_empty() {
            return bad("a build needs the npm registry on its allow list");
        }
        if self.namespace.is_empty() || self.runtime_class.is_empty() {
            return bad("the namespace and the runtime class must be set");
        }
        OciDigest::parse(&self.base_index)?;
        Ok(())
    }
}

/// Why the build channel refuses a request. The server answers `404`, `403`, `409`, `413` or
/// `503` without a detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelRefusal {
    /// No running build of that ID.
    NotFound,
    /// A wrong token, or a caller that is not the build's pod.
    Forbidden,
    /// The source was given or the output taken already.
    Conflict,
    /// The output is over the cap (the build fails).
    TooLarge,
    /// The pod's address is not known yet; the caller may try again.
    Unavailable,
}

impl ChannelRefusal {
    pub fn status(self) -> u16 {
        match self {
            ChannelRefusal::NotFound => 404,
            ChannelRefusal::Forbidden => 403,
            ChannelRefusal::Conflict => 409,
            ChannelRefusal::TooLarge => 413,
            ChannelRefusal::Unavailable => 503,
        }
    }
}

/// The end of a build's output transfer.
enum OutputEnd {
    Done(Bytes),
    OverCap,
}

/// The channel state of one running build.
struct Entry {
    pod: String,
    token_sha: [u8; 32],
    source: Mutex<Option<Bytes>>,
    peer: Mutex<Option<IpAddr>>,
    output: Mutex<Option<tokio::sync::oneshot::Sender<OutputEnd>>>,
}

/// An IPv4-mapped IPv6 address as IPv4.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

/// A successful answer's body, read under `max` bytes (Kimi I6-13, finding 10).
async fn capped(req: reqwest::RequestBuilder, max: usize) -> Result<Bytes, String> {
    let mut r = req.send().await.map_err(|e| e.without_url().to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status()));
    }
    if r.content_length().is_some_and(|n| n > max as u64) {
        return Err(format!("the answer is over {max} bytes"));
    }
    let mut out = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|e| e.without_url().to_string())? {
        if out.len() + chunk.len() > max {
            return Err(format!("the answer is over {max} bytes"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(out))
}

/// The name of a build's pod.
pub fn pod_name(build: uuid::Uuid) -> String {
    format!("ruum-b-{}", build.simple())
}

/// The container security context of a build pod.
fn container_security() -> Value {
    json!({
        "allowPrivilegeEscalation": false,
        "privileged": false,
        "readOnlyRootFilesystem": true,
        "runAsNonRoot": true,
        "runAsUser": BUILD_UID,
        "runAsGroup": BUILD_UID,
        "capabilities": { "drop": ["ALL"] },
        "seccompProfile": { "type": "RuntimeDefault" },
    })
}

/// What a build pod is made of besides the settings.
pub struct PodInput<'a> {
    pub identity: &'a WorkloadIdentity,
    pub build: uuid::Uuid,
    /// The base image reference (`<node registry>/<base repository>@<manifest digest>`).
    pub image: &'a str,
    /// The build's token (the channel's bearer).
    pub token: &'a str,
    /// The channel's base URL of this build (`https://<ip>:<port>/v1/builds/<build>`).
    pub channel: &'a str,
    /// The resolved address of the egress proxy (its `/etc/hosts` line).
    pub proxy_ip: Option<&'a str>,
    pub build_command: Option<&'a str>,
    pub limits: ruum_core::runtime::Limits,
    pub timeout: Duration,
}

/// The pod of one build (see the module docs).
pub fn pod_body(cfg: &KubeBuilderConfig, p: &PodInput<'_>) -> Value {
    let mut env = vec![
        json!({ "name": "HOME", "value": "/tmp" }),
        json!({ "name": "npm_config_cache", "value": "/tmp/.npm" }),
        json!({ "name": "npm_config_registry", "value": cfg.npm_registry }),
        json!({ "name": "npm_config_update_notifier", "value": "false" }),
        json!({ "name": "NODE_OPTIONS", "value": "" }),
        json!({ "name": "RUUM_BUILD_CHANNEL", "value": p.channel }),
        json!({ "name": "RUUM_BUILD_TOKEN", "value": p.token }),
        json!({ "name": "RUUM_BUILD_PIN", "value": cfg.channel_tls.pin }),
        json!({ "name": "RUUM_BUILD_CA", "value": cfg.channel_tls.pem() }),
    ];
    if let Some(c) = p.build_command {
        env.push(json!({ "name": "RUUM_BUILD_COMMAND", "value": c }));
    }
    let mut hosts: Vec<Value> = Vec::new();
    if let (Some(proxy), Some(ip)) = (&cfg.egress_proxy, p.proxy_ip) {
        let url = format!("http://{}:{}", proxy.alias, proxy.port);
        for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            env.push(json!({ "name": name, "value": url }));
        }
        let no = format!("localhost,127.0.0.1,{}", cfg.channel_address);
        env.push(json!({ "name": "NO_PROXY", "value": no }));
        env.push(json!({ "name": "no_proxy", "value": no }));
        hosts.push(json!({ "ip": ip, "hostnames": [proxy.alias] }));
    }
    let mut labels: BTreeMap<String, String> = cfg.extra_labels.clone();
    labels.insert(
        crate::LABEL_MANAGED.to_owned(),
        super::MANAGED_BY.to_owned(),
    );
    labels.insert(LABEL_BUILD.to_owned(), p.build.to_string());
    labels.insert(
        crate::LABEL_TENANT.to_owned(),
        p.identity.tenant.0.to_string(),
    );
    labels.insert(crate::LABEL_APP.to_owned(), p.identity.app.0.to_string());
    labels.insert(
        crate::LABEL_DEPLOYMENT.to_owned(),
        p.identity.deployment.to_string(),
    );
    labels.insert(
        crate::LABEL_ENV.to_owned(),
        p.identity.environment.as_str().to_owned(),
    );
    let limits = json!({
        "cpu": format!("{}m", p.limits.cpu_millis),
        "memory": p.limits.memory_bytes.to_string(),
    });
    let mut spec = json!({
        "runtimeClassName": cfg.runtime_class,
        "serviceAccountName": cfg.service_account,
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "hostNetwork": false,
        "hostPID": false,
        "hostIPC": false,
        "shareProcessNamespace": false,
        "restartPolicy": "Never",
        "activeDeadlineSeconds": p.timeout.as_secs().max(1),
        "terminationGracePeriodSeconds": 0,
        "dnsPolicy": "None",
        "dnsConfig": { "nameservers": ["127.0.0.1"] },
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": BUILD_UID,
            "runAsGroup": BUILD_UID,
            "fsGroup": BUILD_UID,
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        "containers": [{
            "name": CONTAINER,
            "image": p.image,
            "imagePullPolicy": "IfNotPresent",
            "command": ["node", "-e", BUILD_SCRIPT],
            "workingDir": "/work",
            "env": env,
            "resources": { "limits": limits, "requests": limits },
            "securityContext": container_security(),
            "volumeMounts": [
                { "name": "work", "mountPath": "/work" },
                { "name": "tmp", "mountPath": "/tmp" },
            ],
            "stdin": false,
            "tty": false,
            "terminationMessagePolicy": "File",
        }],
        "volumes": [
            { "name": "work", "emptyDir": { "sizeLimit": p.limits.disk_bytes.to_string() } },
            { "name": "tmp", "emptyDir": { "sizeLimit": p.limits.disk_bytes.to_string() } },
        ],
    });
    if !hosts.is_empty() {
        spec["hostAliases"] = json!(hosts);
    }
    if let Some(nodes) = &cfg.node_selector {
        spec["nodeSelector"] = json!(nodes);
    }
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": pod_name(p.build), "labels": labels },
        "spec": spec,
    })
}

/// The builder (see the module docs).
pub struct KubeBuilder {
    api: Arc<Api>,
    cfg: KubeBuilderConfig,
    usage: Arc<dyn RuntimeUsageSink>,
    egress: Arc<dyn EgressRegistry>,
    slots: Slots,
    base_manifest: OciDigest,
    builds: Arc<Mutex<HashMap<uuid::Uuid, Arc<Entry>>>>,
}

/// Why a pod that has not ended will never run: an image or configuration error.
fn broken_reason(pod: &Value) -> Option<String> {
    let statuses = pod.pointer("/status/containerStatuses")?.as_array()?;
    for s in statuses {
        let reason = s.pointer("/state/waiting/reason").and_then(Value::as_str)?;
        if matches!(
            reason,
            "ErrImagePull"
                | "ImagePullBackOff"
                | "InvalidImageName"
                | "ErrImageNeverPull"
                | "CreateContainerConfigError"
                | "CreateContainerError"
        ) {
            return Some(reason.to_owned());
        }
    }
    None
}

/// The exit code of the build container of an ended pod.
fn exit_code(pod: &Value) -> Option<i64> {
    pod.pointer("/status/containerStatuses")?
        .as_array()?
        .iter()
        .find_map(|s| {
            s.pointer("/state/terminated/exitCode")
                .and_then(Value::as_i64)
        })
}

/// The cleanup of one build: the pod (its log read first by the caller), the egress
/// registration and the channel entry. A drop of an unfinished build spawns it.
struct Finish {
    api: Arc<Api>,
    egress: Arc<dyn EgressRegistry>,
    usage: Arc<dyn RuntimeUsageSink>,
    builds: Arc<Mutex<HashMap<uuid::Uuid, Arc<Entry>>>>,
    build: uuid::Uuid,
    identity: WorkloadIdentity,
    begun: Instant,
    registered: Arc<Mutex<bool>>,
    slot: Option<Slot>,
    done: bool,
}

async fn cleanup(
    api: &Api,
    egress: &dyn EgressRegistry,
    builds: &Mutex<HashMap<uuid::Uuid, Arc<Entry>>>,
    build: uuid::Uuid,
    registered: &Mutex<bool>,
) {
    builds
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&build);
    let pod = pod_name(build);
    // The egress registration goes first: while the pod exists its address cannot go to another
    // pod. When the proxy cannot be reached the pod stays, and the sweep unregisters and deletes
    // it later (Kimi I6-13, finding 9).
    let was = *registered.lock().unwrap_or_else(|p| p.into_inner());
    if was {
        let mut ok = false;
        for attempt in 0..3u64 {
            match egress.unregister(&pod).await {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(target: "ruum::runtime", %build, code = e.code(), "a build's egress registration could not be removed");
                    tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                }
            }
        }
        if !ok {
            tracing::warn!(target: "ruum::runtime", %build, "the build pod stays until the sweep removes its egress registration");
            return;
        }
        *registered.lock().unwrap_or_else(|p| p.into_inner()) = false;
    }
    if let Err(e) = api.delete(Kind::Pod, &pod, Some(0)).await {
        tracing::warn!(target: "ruum::runtime", %build, code = e.code(), "a build pod could not be deleted; the next sweep of the namespace must");
    }
}

impl Finish {
    async fn run(&mut self) -> Duration {
        cleanup(
            &self.api,
            self.egress.as_ref(),
            &self.builds,
            self.build,
            &self.registered,
        )
        .await;
        let took = self.begun.elapsed().max(Duration::from_millis(1));
        self.usage.record(RuntimeUsage::Build {
            identity: self.identity,
            took,
        });
        self.done = true;
        self.slot = None;
        took
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let api = self.api.clone();
        let egress = self.egress.clone();
        let usage = self.usage.clone();
        let builds = self.builds.clone();
        let registered = self.registered.clone();
        let (build, identity, begun) = (self.build, self.identity, self.begun);
        let slot = self.slot.take();
        let record = move || {
            usage.record(RuntimeUsage::Build {
                identity,
                took: begun.elapsed().max(Duration::from_millis(1)),
            });
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    cleanup(&api, egress.as_ref(), &builds, build, &registered).await;
                    record();
                    drop(slot);
                });
            }
            Err(_) => {
                builds
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&build);
                tracing::warn!(target: "ruum::runtime", %build, "a build was dropped outside a runtime: its pod is left");
                record();
            }
        }
    }
}

impl KubeBuilder {
    /// Checks the settings and picks the base image of the nodes' platform from the embedded
    /// index (the only pin this builder accepts; another pin needs the Docker builder's
    /// registry read). Makes no API call.
    pub fn new(
        cfg: KubeBuilderConfig,
        usage: Arc<dyn RuntimeUsageSink>,
        egress: Arc<dyn EgressRegistry>,
    ) -> Result<Self, RuntimeError> {
        cfg.check()?;
        if cfg.base_index != NODE22_BASE_INDEX
            || OciDigest::of(super::EMBEDDED_INDEX).as_str() != NODE22_BASE_INDEX
        {
            return Err(RuntimeError::Internal {
                reason: "the Kubernetes builder builds on the embedded node22 index only"
                    .to_owned(),
            });
        }
        let arch = oci::engine_architecture(&cfg.node_arch)?;
        let d = oci::select_manifest(super::EMBEDDED_INDEX, arch)?;
        let base_manifest = OciDigest::parse(&d.digest)?;
        let api = Api::new(
            &cfg.api_url,
            &cfg.namespace,
            cfg.token_file.clone(),
            cfg.ca_file.as_deref(),
            cfg.api_timeout,
        )?;
        Ok(Self {
            api: Arc::new(api),
            slots: Slots::new(cfg.max_parallel, cfg.per_tenant),
            cfg,
            usage,
            egress,
            base_manifest,
            builds: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// The build pod's image: the base image by its platform manifest's digest.
    pub fn base_image(&self) -> String {
        format!(
            "{}/{}@{}",
            self.cfg.node_registry, self.cfg.base_repository, self.base_manifest
        )
    }

    /// The settings.
    pub fn config(&self) -> &KubeBuilderConfig {
        &self.cfg
    }

    /// The channel's base URL as the pods reach it (the address resolved here: the pods have
    /// no DNS).
    async fn channel_base(&self) -> Result<String, RuntimeError> {
        let a = &self.cfg.channel_address;
        let ip = match a.parse::<IpAddr>() {
            Ok(ip) => ip,
            Err(_) => tokio::net::lookup_host((a.as_str(), self.cfg.channel_port))
                .await
                .map_err(|e| RuntimeError::Unavailable {
                    reason: format!("the build channel's address does not resolve: {e}"),
                })?
                .next()
                .map(|s| s.ip())
                .ok_or_else(|| RuntimeError::Unavailable {
                    reason: "the build channel's address does not resolve".to_owned(),
                })?,
        };
        Ok(match ip {
            IpAddr::V6(v6) => format!("https://[{v6}]:{}", self.cfg.channel_port),
            IpAddr::V4(v4) => format!("https://{v4}:{}", self.cfg.channel_port),
        })
    }

    async fn proxy_ip(&self) -> Result<Option<String>, RuntimeError> {
        let Some(p) = &self.cfg.egress_proxy else {
            return Ok(None);
        };
        if let Ok(ip) = p.address.parse::<IpAddr>() {
            return Ok(Some(ip.to_string()));
        }
        let ip = tokio::net::lookup_host((p.address.as_str(), p.port))
            .await
            .map_err(|e| RuntimeError::Unavailable {
                reason: format!("the egress proxy's address does not resolve: {e}"),
            })?
            .next()
            .map(|s| s.ip().to_string())
            .ok_or_else(|| RuntimeError::Unavailable {
                reason: "the egress proxy's address does not resolve".to_owned(),
            })?;
        Ok(Some(ip))
    }

    fn entry(&self, build: &str) -> Option<Arc<Entry>> {
        let id = uuid::Uuid::parse_str(build).ok()?;
        self.builds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned()
    }

    /// The token and the caller's address of a channel request.
    async fn admit(
        &self,
        build: &str,
        bearer: &str,
        peer: IpAddr,
    ) -> Result<Arc<Entry>, ChannelRefusal> {
        let e = self.entry(build).ok_or(ChannelRefusal::NotFound)?;
        let sha: [u8; 32] = Sha256::digest(bearer.as_bytes()).into();
        if sha != e.token_sha {
            tracing::warn!(target: "ruum::security", event = "build_channel.refused", reason = "token", "a build channel request was refused");
            return Err(ChannelRefusal::Forbidden);
        }
        let known = *e.peer.lock().unwrap_or_else(|p| p.into_inner());
        let ip = match known {
            Some(ip) => ip,
            None => {
                let pod = self
                    .api
                    .get(Kind::Pod, &e.pod)
                    .await
                    .map_err(|_| ChannelRefusal::Unavailable)?
                    .ok_or(ChannelRefusal::NotFound)?;
                let ip = pod
                    .pointer("/status/podIP")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse::<IpAddr>().ok())
                    .ok_or(ChannelRefusal::Unavailable)?;
                *e.peer.lock().unwrap_or_else(|p| p.into_inner()) = Some(canonical(ip));
                canonical(ip)
            }
        };
        if canonical(peer) != ip {
            tracing::warn!(target: "ruum::security", event = "build_channel.refused", reason = "peer", "a build channel request was refused");
            return Err(ChannelRefusal::Forbidden);
        }
        Ok(e)
    }

    /// `GET /v1/builds/<build>/source` of the build channel: the source tar, once.
    pub async fn channel_source(
        &self,
        build: &str,
        bearer: &str,
        peer: IpAddr,
    ) -> Result<Bytes, ChannelRefusal> {
        let e = self.admit(build, bearer, peer).await?;
        e.source
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .ok_or(ChannelRefusal::Conflict)
    }

    /// `PUT /v1/builds/<build>/output` of the build channel: the output tar, once, read under
    /// the cap. Over the cap the build fails and the answer is [`ChannelRefusal::TooLarge`].
    pub async fn channel_output<S, E>(
        &self,
        build: &str,
        bearer: &str,
        peer: IpAddr,
        mut body: S,
    ) -> Result<(), ChannelRefusal>
    where
        S: futures::Stream<Item = Result<Bytes, E>> + Unpin + Send,
        E: std::fmt::Display,
    {
        use futures::StreamExt;
        let e = self.admit(build, bearer, peer).await?;
        let tx = e
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .ok_or(ChannelRefusal::Conflict)?;
        let cap = self.cfg.output_max;
        let mut out = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(err) => {
                    // A cut upload is no output: the build fails when its pod ends.
                    tracing::warn!(target: "ruum::runtime", error = %err, "a build's output upload was cut");
                    return Err(ChannelRefusal::Unavailable);
                }
            };
            if out.len() as u64 + chunk.len() as u64 > cap {
                let _ = tx.send(OutputEnd::OverCap);
                return Err(ChannelRefusal::TooLarge);
            }
            out.extend_from_slice(&chunk);
        }
        tx.send(OutputEnd::Done(Bytes::from(out)))
            .map_err(|_| ChannelRefusal::NotFound)
    }

    /// Copies the base image of the nodes' platform (the pinned index's entry) from Docker Hub
    /// into the cluster registry's base repository, byte for byte, unless it is there already
    /// (P-84 "mirror ruum-base/node22"). Every blob and the manifest are checked against their
    /// digests; the push uses the builder's credential. The build pods and the assembly read the
    /// base from there only. `hub` is the Docker Hub registry (`https://registry-1.docker.io`),
    /// `auth` its token service.
    pub async fn mirror_base(&self, hub: &str, auth: &str) -> Result<bool, RuntimeError> {
        let push = registry::RepositoryPush::new(
            &self.cfg.registry_url,
            self.cfg.base_repository.clone(),
            self.cfg.base_repository.clone(),
            self.cfg.credential.clone(),
            self.cfg.token_realm.as_deref(),
        )?;
        if push.base_manifest(&self.base_manifest).await.is_ok() {
            return Ok(false);
        }
        let arch = oci::engine_architecture(&self.cfg.node_arch)?;
        let d = oci::select_manifest(super::EMBEDDED_INDEX, arch)?;
        let un = |what: &str, e: &dyn std::fmt::Display| RuntimeError::Unavailable {
            reason: format!("mirroring the base image ({what}): {e}"),
        };
        let c = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| un("client", &e))?;
        #[derive(serde::Deserialize)]
        struct Tok {
            token: String,
        }
        let tok = capped(
            c.get(format!(
                "{auth}?service=registry.docker.io&scope=repository:library/node:pull"
            )),
            MIRROR_SMALL_MAX,
        )
        .await
        .map_err(|e| un("token", &e))?;
        let tok: Tok = serde_json::from_slice(&tok).map_err(|e| un("token", &e))?;
        let from = format!("{}/v2/library/node", hub.trim_end_matches('/'));
        let manifest = capped(
            c.get(format!("{from}/manifests/{}", d.digest))
                .bearer_auth(&tok.token)
                .header("Accept", &d.media_type),
            MIRROR_SMALL_MAX,
        )
        .await
        .map_err(|e| un("manifest", &e))?;
        if OciDigest::of(&manifest) != self.base_manifest {
            return Err(un("manifest", &"the manifest does not match the pin"));
        }
        let m = oci::parse_manifest(&manifest)?;
        for b in std::iter::once(&m.config).chain(m.layers.iter()) {
            // Exactly the size the (pinned) manifest gives, never more.
            let size = usize::try_from(b.size).map_err(|e| un("blob", &e))?;
            let data = capped(
                c.get(format!("{from}/blobs/{}", b.digest))
                    .bearer_auth(&tok.token),
                size,
            )
            .await
            .map_err(|e| un("blob", &e))?;
            if OciDigest::of(&data).as_str() != b.digest {
                return Err(un(
                    "blob",
                    &format!("{} does not match its digest", b.digest),
                ));
            }
            push.push_blob(&OciDigest::parse(&b.digest)?, data).await?;
        }
        let got = push.push_manifest(&d.media_type, manifest).await?;
        if got != self.base_manifest {
            return Err(un("manifest", &"the registry stored another digest"));
        }
        Ok(true)
    }

    /// The pods of builds that this or another server process left behind: the pods of the
    /// builder in the namespace that no running build of this process owns and that are older
    /// than `age`. Deletes them; the count. (A server that restarts mid-build leaves its pod.)
    pub async fn sweep(&self, age: Duration) -> Result<usize, RuntimeError> {
        let pods = self
            .api
            .list(Kind::Pod, &[(crate::LABEL_MANAGED, super::MANAGED_BY)])
            .await?;
        let mine: Vec<String> = self
            .builds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(|e| e.pod.clone())
            .collect();
        let now = time::OffsetDateTime::now_utc();
        let mut deleted = 0;
        for p in pods {
            let Some(name) = p.pointer("/metadata/name").and_then(Value::as_str) else {
                continue;
            };
            if mine.iter().any(|m| m == name) || !name.starts_with("ruum-b-") {
                continue;
            }
            let created = p
                .pointer("/metadata/creationTimestamp")
                .and_then(Value::as_str)
                .and_then(|t| {
                    time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                        .ok()
                });
            if created.is_some_and(|c| now - c < age) {
                continue;
            }
            // Its egress registration first (a no-op when there is none), then the pod.
            if self.cfg.egress_proxy.is_some() && self.egress.unregister(name).await.is_err() {
                continue;
            }
            if self.api.delete(Kind::Pod, name, Some(0)).await.is_ok() {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// The build up to its manifest push; the caller times it out, commits and cleans up.
    async fn run(
        &self,
        input: &BuildInput,
        build: uuid::Uuid,
        registered: &Mutex<bool>,
        log: &Mutex<job::LogTail>,
    ) -> Result<Ready, RuntimeError> {
        let (recipe, source) = prepare(input, self.cfg.output_max).await?;
        let token = hex::encode(rand::random::<[u8; 32]>());
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let pod = pod_name(build);
        let entry = Arc::new(Entry {
            pod: pod.clone(),
            token_sha: Sha256::digest(token.as_bytes()).into(),
            source: Mutex::new(Some(source)),
            peer: Mutex::new(None),
            output: Mutex::new(Some(tx)),
        });
        self.builds
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(build, entry.clone());
        let channel = format!("{}/v1/builds/{build}", self.channel_base().await?);
        let proxy_ip = self.proxy_ip().await?;
        let image = self.base_image();
        let body = pod_body(
            &self.cfg,
            &PodInput {
                identity: &input.identity,
                build,
                image: &image,
                token: &token,
                channel: &channel,
                proxy_ip: proxy_ip.as_deref(),
                build_command: recipe.build.as_deref(),
                limits: input.limits,
                timeout: input.timeout,
            },
        );
        match self.api.create(Kind::Pod, &body).await? {
            Write::Done(_) => {}
            _ => {
                return Err(RuntimeError::Internal {
                    reason: "a build pod of that name exists".to_owned(),
                });
            }
        }
        let mut output: Option<Bytes> = None;
        let mut ended_at: Option<Instant> = None;
        loop {
            if output.is_none() {
                tokio::select! {
                    r = &mut rx => match r {
                        Ok(OutputEnd::Done(b)) => output = Some(b),
                        Ok(OutputEnd::OverCap) => {
                            return Err(failed(format!(
                                "the build's output is over the cap of {} bytes",
                                self.cfg.output_max
                            )));
                        }
                        Err(_) => {
                            return Err(failed("the upload of the build's output was cut"));
                        }
                    },
                    () = tokio::time::sleep(POLL) => {}
                }
            } else {
                tokio::time::sleep(POLL / 4).await;
            }
            let Some(p) = self.api.get(Kind::Pod, &pod).await? else {
                return Err(failed("the build pod is gone (deleted or evicted)"));
            };
            if let Some(ip) = p
                .pointer("/status/podIP")
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<IpAddr>().ok())
            {
                let first = {
                    let mut known = entry.peer.lock().unwrap_or_else(|p| p.into_inner());
                    let first = known.is_none();
                    *known = Some(canonical(ip));
                    first
                };
                if first && self.cfg.egress_proxy.is_some() {
                    *registered.lock().unwrap_or_else(|p| p.into_inner()) = true;
                    let subnet = if ip.is_ipv6() {
                        format!("{ip}/128")
                    } else {
                        format!("{ip}/32")
                    };
                    self.egress
                        .register(&EgressNetwork {
                            network: pod.clone(),
                            subnet,
                            identity: input.identity,
                            // The npm registry only, as the Docker builder (Kimi I6-13, 6).
                            allow: Some(self.cfg.npm_hosts.clone()),
                        })
                        .await?;
                }
            }
            if let Some(reason) = broken_reason(&p) {
                return Err(RuntimeError::Unavailable {
                    reason: format!("the build pod cannot start: {reason}"),
                });
            }
            match p.pointer("/status/phase").and_then(Value::as_str) {
                Some("Succeeded") => {
                    if output.is_none()
                        && let Ok(OutputEnd::Done(b)) = rx.try_recv()
                    {
                        output = Some(b);
                    }
                    if let Some(o) = output.take() {
                        self.read_log(&pod, log).await;
                        let ready = assemble(
                            &Assembly {
                                registry_url: &self.cfg.registry_url,
                                base_repository: &self.cfg.base_repository,
                                base_manifest: &self.base_manifest,
                                credential: self.cfg.credential.clone(),
                                token_realm: self.cfg.token_realm.as_deref(),
                                extra_labels: &self.cfg.extra_labels,
                                output_max: self.cfg.output_max,
                            },
                            input,
                            build,
                            &recipe,
                            o,
                            log,
                        )
                        .await?;
                        return Ok(ready);
                    }
                    let at = *ended_at.get_or_insert_with(Instant::now);
                    if at.elapsed() > OUTPUT_GRACE {
                        self.read_log(&pod, log).await;
                        return Err(failed(format!(
                            "the build ended without an output: {}",
                            tail_text(log)
                        )));
                    }
                }
                Some("Failed") => {
                    self.read_log(&pod, log).await;
                    let code = exit_code(&p)
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "unknown".to_owned());
                    let why = p
                        .pointer("/status/reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if why == "DeadlineExceeded" {
                        return Err(RuntimeError::BuildTimedOut);
                    }
                    return Err(failed(format!(
                        "the build exited with code {code}: {}",
                        tail_text(log)
                    )));
                }
                _ => {}
            }
        }
    }

    /// Reads the end of the build log into `log` (best effort).
    async fn read_log(&self, pod: &str, log: &Mutex<job::LogTail>) {
        match self
            .api
            .log_tail(pod, CONTAINER, LOG_BYTES, MAX_BUILD_LOG_LINES as u64)
            .await
        {
            Ok(Some(b)) => {
                let mut l = log.lock().unwrap_or_else(|p| p.into_inner());
                l.push_bytes(&b);
                l.push_bytes(b"\n");
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(target: "ruum::runtime", code = e.code(), "the build log could not be read")
            }
        }
    }
}

#[async_trait]
impl Builder for KubeBuilder {
    fn name(&self) -> &str {
        "kubernetes-gvisor-node22"
    }

    async fn build(&self, input: BuildInput) -> Result<BuildOutput, RuntimeError> {
        input.validate()?;
        if input.runtime != "node22" {
            return Err(RuntimeError::UnsupportedRuntime {
                runtime: input.runtime,
            });
        }
        // A bounded wait (review I6-13, 1): the deployment holds a deploy slot meanwhile.
        let Some(slot) = self
            .slots
            .acquire_within(input.identity.tenant, self.cfg.slot_wait)
            .await
        else {
            return Err(RuntimeError::Unavailable {
                reason: BUILD_SLOTS_BUSY.to_owned(),
            });
        };
        let build = uuid::Uuid::now_v7();
        let registered = Arc::new(Mutex::new(false));
        let mut finish = Finish {
            api: self.api.clone(),
            egress: self.egress.clone(),
            usage: self.usage.clone(),
            builds: self.builds.clone(),
            build,
            identity: input.identity,
            begun: Instant::now(),
            registered: registered.clone(),
            slot: Some(slot),
            done: false,
        };
        let log = Mutex::new(job::LogTail::new(
            MAX_BUILD_LOG_LINES,
            super::MAX_BUILD_LOG_LINE_BYTES,
        ));
        let ready =
            match tokio::time::timeout(input.timeout, self.run(&input, build, &registered, &log))
                .await
            {
                Ok(r) => r,
                Err(_) => Err(RuntimeError::BuildTimedOut),
            };
        // The commit point, outside the timeout (as the Docker builder).
        let result = match ready {
            Ok(r) => r
                .push
                .push_manifest(registry::OCI_MANIFEST, r.manifest)
                .await
                .inspect(|digest| {
                    log.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .note(&format!("ruum-build: image {digest}"));
                }),
            Err(e) => Err(e),
        };
        let took = finish.run().await;
        let digest = result?;
        let logs = log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .lines()
            .into_iter()
            .map(|(at, text)| BuildLogLine { at, text })
            .collect();
        Ok(BuildOutput {
            image: ImageRef::Oci(digest),
            logs,
            took,
            routes: BUILD_ROUTES.to_vec(),
            limits: input.limits,
        })
    }
}

#[cfg(test)]
#[path = "kube_tests.rs"]
mod tests;
