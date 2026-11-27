//! The Kubernetes objects of one instance: the record (a ConfigMap), the pod of each run and
//! what the driver reads back from them.
//!
//! Per instance `ruum-i-<id>` (the ID without dashes):
//!
//! - **ConfigMap `ruum-i-<id>`**, key `record`: the [`Record`] (identity, kind, routes, limits,
//!   image, command, plain environment, state, run) and its `sealed` part (backlog S-89,
//!   [`super::seal`]): the secret values, the previous values, the workload token, the app's
//!   command, a digest of the fixed fields, and the run and pod `uid` that may take the launch
//!   data. It is the instance's state, shared by every server process: any of them answers
//!   `status`, `stop`, `wake` and `touch`, also after a restart (adoption, backlog R2A-1). State
//!   changes replace it with its `resourceVersion`, so of two processes only one makes a change
//!   (and its effects: usage, unregistration).
//! - **Pod `ruum-i-<id>-<run>`**: one per run (a cold start makes a new pod). The sandbox
//!   supervisor (backlog I6-20) is its first process; it fetches the launch data from the server
//!   with the pod's projected service account token.
//!
//! No Secret: the runtime identity has no verb on Secrets (`docs/STEP3.md` "The sandbox
//! supervisor").

use super::KubeConfig;
use crate::driver::{LABEL_APP, LABEL_DEPLOYMENT, LABEL_ENV, LABEL_INSTANCE, LABEL_TENANT};
use ruum_core::runtime::{
    EnvValue, ExitReason, ExitStatus, InstanceId, InstanceState, Limits, Route, TOKEN_FILE,
    TOKEN_FILE_ENV, WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use ruum_supervisor::protocol::{
    IDENTITY_DIR, IDENTITY_FILE, SUPERVISOR_DIR, SUPERVISOR_PATH, TERMINATION_LOG,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use time::OffsetDateTime;

/// The label of the run on a pod.
pub const LABEL_RUN: &str = "ruum.run";
/// The container name in every pod.
pub(crate) const CONTAINER: &str = "app";
/// The user and group every sandbox runs as (the image's `USER` is ignored).
pub(crate) const SANDBOX_UID: i64 = 10001;

/// The name of an instance's record (a ConfigMap).
pub fn object_name(id: InstanceId) -> String {
    format!("ruum-i-{}", id.0.simple())
}

/// The name of the pod of one run.
pub fn pod_name(id: InstanceId, run: u32) -> String {
    format!("{}-{run}", object_name(id))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HealthRec {
    pub path: String,
    pub interval_ms: u64,
    pub failure_threshold: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum KindRec {
    Service {
        port: u16,
        health: Option<HealthRec>,
        idle_timeout_ms: Option<u64>,
    },
    Job {
        timeout_ms: u64,
    },
}

impl KindRec {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            KindRec::Service { .. } => "service",
            KindRec::Job { .. } => "job",
        }
    }
}

/// One environment variable: a plain value, or (`value: None`) a secret value (sealed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EnvRec {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// The state of an instance, as the ConfigMap holds it. No secret value is in it in the clear:
/// the values are in [`Record::sealed`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub v: u32,
    pub instance: InstanceId,
    pub identity: WorkloadIdentity,
    pub kind: KindRec,
    pub routes: Vec<Route>,
    pub limits: Limits,
    /// The image reference (`<registry>/ruum/<tenant>/<app>@<digest>`).
    pub image: String,
    pub command: Vec<String>,
    pub env: Vec<EnvRec>,
    pub started_at_ms: i64,
    pub state: InstanceState,
    /// The run (1 for the first; each cold start adds one).
    pub run: u32,
    pub run_started_ms: i64,
    pub last_touch_ms: i64,
    /// When the instance ended (stopped or exited): the start of its retention.
    #[serde(default)]
    pub ended_ms: Option<i64>,
    /// The pod whose address is registered at the egress proxy (the registration key). It is
    /// cleared only after the unregistration passed; until then the pod is not deleted, so its
    /// address is not given to another pod while the proxy still names this app for it.
    #[serde(default)]
    pub registered: Option<String>,
    /// The sealed part ([`super::seal`]); `None` until the first pod of the instance exists.
    #[serde(default)]
    pub sealed: Option<String>,
    /// Fields this release does not know (design 12.7, decision 42): a record that a newer
    /// release wrote keeps them when this release writes it back during a rolling update.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

pub(crate) fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub(crate) fn from_ms(ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

impl Record {
    pub(crate) fn new(spec: &WorkloadSpec, id: InstanceId, image: String) -> Self {
        let kind = match &spec.kind {
            WorkloadKind::Service(s) => KindRec::Service {
                port: s.port,
                health: s.health.as_ref().map(|h| HealthRec {
                    path: h.path.clone(),
                    interval_ms: h.interval.as_millis().min(u128::from(u64::MAX)) as u64,
                    failure_threshold: h.failure_threshold,
                }),
                idle_timeout_ms: s
                    .idle_timeout
                    .map(|d| d.as_millis().min(u128::from(u64::MAX)) as u64),
            },
            WorkloadKind::Job(j) => KindRec::Job {
                timeout_ms: j.timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            },
            // check_spec refuses other kinds before a record is made.
            _ => KindRec::Job { timeout_ms: 1 },
        };
        let mut routes = spec.routes.clone();
        routes.sort();
        let now = now_ms();
        Self {
            v: 1,
            instance: id,
            identity: spec.identity,
            kind,
            routes,
            limits: spec.limits,
            image,
            command: spec.command.clone(),
            env: spec
                .env
                .iter()
                .map(|v| EnvRec {
                    name: v.name.clone(),
                    value: match &v.value {
                        EnvValue::Plain(p) => Some(p.clone()),
                        EnvValue::Secret(_) => None,
                    },
                })
                .collect(),
            started_at_ms: now,
            state: InstanceState::Starting,
            run: 1,
            run_started_ms: now,
            last_touch_ms: now,
            ended_ms: None,
            registered: None,
            sealed: None,
            extra: serde_json::Map::new(),
        }
    }
}

/// The labels of every object of an instance (the record and each pod).
pub(crate) fn labels(cfg: &KubeConfig, identity: &WorkloadIdentity, id: InstanceId) -> Value {
    let mut l: BTreeMap<String, String> = cfg.extra_labels.clone();
    l.insert(super::LABEL_MANAGED.to_owned(), super::MANAGED.to_owned());
    l.insert(LABEL_TENANT.to_owned(), identity.tenant.0.to_string());
    l.insert(LABEL_APP.to_owned(), identity.app.0.to_string());
    l.insert(
        LABEL_ENV.to_owned(),
        identity.environment.as_str().to_owned(),
    );
    l.insert(LABEL_DEPLOYMENT.to_owned(), identity.deployment.to_string());
    l.insert(LABEL_INSTANCE.to_owned(), id.0.to_string());
    json!(l)
}

pub(crate) fn record_body(cfg: &KubeConfig, rec: &Record, resource_version: Option<&str>) -> Value {
    let mut meta = json!({
        "name": object_name(rec.instance),
        "labels": labels(cfg, &rec.identity, rec.instance),
    });
    if let Some(rv) = resource_version {
        meta["resourceVersion"] = json!(rv);
    }
    json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": meta,
        "data": { "record": serde_json::to_string(rec).unwrap_or_default() },
    })
}

/// The record and the resourceVersion of a ConfigMap; `None` when it is not one of the driver's.
pub(crate) fn read_record(cm: &Value) -> Option<(Record, String)> {
    let text = cm.pointer("/data/record")?.as_str()?;
    let rec: Record = serde_json::from_str(text).ok()?;
    let rv = cm
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // The labels must say what the record says: a record copied under another tenant's labels
    // is not taken.
    let label = |k: &str| cm.pointer(&format!("/metadata/labels/{}", k.replace('/', "~1")));
    let tenant_ok = label(LABEL_TENANT).and_then(Value::as_str)
        == Some(rec.identity.tenant.0.to_string().as_str());
    let inst_ok =
        label(LABEL_INSTANCE).and_then(Value::as_str) == Some(rec.instance.0.to_string().as_str());
    let name_ok = cm.pointer("/metadata/name").and_then(Value::as_str)
        == Some(object_name(rec.instance).as_str());
    (tenant_ok && inst_ok && name_ok).then_some((rec, rv))
}

/// The SHA-256 (hex) of a record's fixed fields: what the record says the instance is, which no
/// state change alters. The sealed part carries it, so a record whose fixed fields were changed
/// (by a writer without the record key) is refused (`docs/STEP3.md` "The sandbox supervisor").
pub(crate) fn fixed_digest(rec: &Record) -> String {
    #[derive(Serialize)]
    struct Fixed<'a> {
        v: u32,
        instance: &'a InstanceId,
        identity: &'a WorkloadIdentity,
        kind: &'a KindRec,
        routes: &'a [Route],
        limits: &'a Limits,
        image: &'a str,
        command: &'a [String],
        env: &'a [EnvRec],
        started_at_ms: i64,
    }
    let f = Fixed {
        v: rec.v,
        instance: &rec.instance,
        identity: &rec.identity,
        kind: &rec.kind,
        routes: &rec.routes,
        limits: &rec.limits,
        image: &rec.image,
        command: &rec.command,
        env: &rec.env,
        started_at_ms: rec.started_at_ms,
    };
    hex::encode(Sha256::digest(serde_json::to_vec(&f).unwrap_or_default()))
}

/// The secret environment values of a spec (name, value).
pub(crate) fn secret_env(spec: &WorkloadSpec) -> Vec<(String, String)> {
    spec.env
        .iter()
        .filter_map(|v| match &v.value {
            EnvValue::Secret(s) => Some((v.name.clone(), s.expose().to_owned())),
            EnvValue::Plain(_) => None,
        })
        .collect()
}

/// A shared service as the pod reaches it: its address on the pod's `/etc/hosts`.
#[derive(Clone, Debug)]
pub(crate) struct HostAlias {
    pub ip: String,
    pub alias: String,
}

fn secs_ceil(ms: u64) -> u64 {
    ms.div_ceil(1000).max(1)
}

/// The folder of the token file (a memory `emptyDir` the supervisor writes).
fn token_dir() -> &'static str {
    TOKEN_FILE
        .rsplit_once('/')
        .map(|(d, _)| d)
        .unwrap_or("/var/run/ruum")
}

/// Where the supervisor binary is in its own image (the init container copies it from there).
pub const SUPERVISOR_IN_IMAGE: &str = "/ruum-supervisor";

/// The pin of the supervisor listener's certificate when the launch channel is TLS
/// (`KubeConfig::supervisor_tls`, backlog I620-2), derived from the record key.
pub(crate) fn supervisor_pin(cfg: &KubeConfig) -> Option<String> {
    if !cfg.supervisor_tls {
        return None;
    }
    let key = cfg.record_key.as_ref()?;
    super::tls::SupervisorTls::derive(key).ok().map(|t| t.pin)
}
/// The name of the init container that installs the supervisor.
pub(crate) const INIT_CONTAINER: &str = "ruum-supervisor";
/// The projected service account token's lifetime (the kubelet renews it at 80 %).
pub(crate) const IDENTITY_EXPIRY_SECS: u64 = 600;

/// The container security context of every container of a sandbox.
fn container_security() -> Value {
    json!({
        "allowPrivilegeEscalation": false,
        "privileged": false,
        "readOnlyRootFilesystem": true,
        "runAsNonRoot": true,
        "runAsUser": SANDBOX_UID,
        "runAsGroup": SANDBOX_UID,
        "capabilities": { "drop": ["ALL"] },
        "seccompProfile": { "type": "RuntimeDefault" },
    })
}

/// The pod of one run: the sandbox of STEP3 "How an app runs" rule 1 as a restricted pod in
/// gVisor, with the sandbox supervisor as its first process (see the crate README "Kubernetes
/// driver" and `docs/STEP3.md` "The sandbox supervisor"). `launch_url` is the server's
/// supervisor listener as `http://<ip>:<port>`.
pub(crate) fn pod_body(
    cfg: &KubeConfig,
    rec: &Record,
    hosts: &[HostAlias],
    launch_url: &str,
) -> Value {
    let id = rec.instance;
    // Plain values only: the secret values come from the supervisor's launch data.
    let mut env: Vec<Value> = rec
        .env
        .iter()
        .filter_map(|e| {
            e.value
                .as_ref()
                .map(|v| json!({ "name": e.name, "value": v }))
        })
        .collect();
    env.push(json!({ "name": TOKEN_FILE_ENV, "value": TOKEN_FILE }));
    if rec.routes.contains(&Route::EgressProxy)
        && let Some(p) = cfg.routes.get(&Route::EgressProxy)
    {
        let proxy = format!("http://{}:{}", p.alias, p.port);
        for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            env.push(json!({ "name": name, "value": proxy }));
        }
        let mut no: Vec<String> = vec!["localhost".to_owned(), "127.0.0.1".to_owned()];
        for r in &rec.routes {
            if *r != Route::EgressProxy
                && let Some(s) = cfg.routes.get(r)
            {
                no.push(s.alias.clone());
            }
        }
        env.push(json!({ "name": "NO_PROXY", "value": no.join(",") }));
        env.push(json!({ "name": "no_proxy", "value": no.join(",") }));
        env.push(json!({ "name": "NODE_USE_ENV_PROXY", "value": "1" }));
    }
    let mut labels = labels(cfg, &rec.identity, id);
    labels[LABEL_RUN] = json!(rec.run.to_string());
    let mut command = vec![
        json!(SUPERVISOR_PATH),
        json!("run"),
        json!("--launch-url"),
        json!(launch_url),
        json!("--identity-file"),
        json!(IDENTITY_FILE),
        json!("--token-file"),
        json!(TOKEN_FILE),
    ];
    // TLS on the launch channel (backlog I620-2): the supervisor accepts only the listener's
    // certificate of this pin, which the read-back holds like every other argument.
    if let Some(pin) = supervisor_pin(cfg) {
        command.extend([json!("--server-pin"), json!(pin)]);
    }
    let mut container = json!({
        "name": CONTAINER,
        "image": rec.image,
        "imagePullPolicy": "IfNotPresent",
        "command": command,
        "env": env,
        "resources": {
            "limits": { "cpu": format!("{}m", rec.limits.cpu_millis), "memory": rec.limits.memory_bytes.to_string() },
            "requests": { "cpu": format!("{}m", rec.limits.cpu_millis), "memory": rec.limits.memory_bytes.to_string() },
        },
        "securityContext": container_security(),
        "volumeMounts": [
            { "name": "tmp", "mountPath": "/tmp" },
            { "name": "run", "mountPath": token_dir() },
            { "name": "ruum-bin", "mountPath": SUPERVISOR_DIR, "readOnly": true },
            { "name": "ruum-id", "mountPath": IDENTITY_DIR, "readOnly": true },
        ],
        "stdin": false,
        "tty": false,
        "terminationMessagePath": TERMINATION_LOG,
        "terminationMessagePolicy": "File",
    });
    if let KindRec::Service { port, health, .. } = &rec.kind {
        container["ports"] = json!([{ "name": "app", "containerPort": port }]);
        if let Some(h) = health {
            // Inside the sandbox (an exec probe), never from the node's network: decision 0006
            // G point 1 and backlog I6-1.
            let mut cmd = cfg.health_command.clone();
            cmd.push(format!("http://127.0.0.1:{port}{}", h.path));
            container["readinessProbe"] = json!({
                "exec": { "command": cmd },
                "periodSeconds": secs_ceil(h.interval_ms),
                "failureThreshold": h.failure_threshold,
                "successThreshold": 1,
                "timeoutSeconds": 5,
            });
        }
    }
    // Copies the supervisor into the read-only volume of the app container.
    let init = json!({
        "name": INIT_CONTAINER,
        "image": cfg.supervisor_image,
        "imagePullPolicy": "IfNotPresent",
        "command": [SUPERVISOR_IN_IMAGE, "install", SUPERVISOR_PATH],
        "resources": {
            "limits": { "cpu": "100m", "memory": "32Mi" },
            "requests": { "cpu": "100m", "memory": "32Mi" },
        },
        "securityContext": container_security(),
        "volumeMounts": [{ "name": "ruum-bin", "mountPath": SUPERVISOR_DIR }],
        "stdin": false,
        "tty": false,
        "terminationMessagePath": TERMINATION_LOG,
        "terminationMessagePolicy": "File",
    });
    let mut spec = json!({
        "runtimeClassName": cfg.runtime_class,
        "serviceAccountName": cfg.sandbox_service_account,
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "hostNetwork": false,
        "hostPID": false,
        "hostIPC": false,
        "shareProcessNamespace": false,
        "restartPolicy": "Never",
        "terminationGracePeriodSeconds": cfg.stop_timeout.as_secs(),
        // No DNS: the sandbox finds the shared services by /etc/hosts (the network policy lets it
        // reach only the egress proxy and the supervisor listener).
        "dnsPolicy": "None",
        "dnsConfig": { "nameservers": ["127.0.0.1"] },
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": SANDBOX_UID,
            "runAsGroup": SANDBOX_UID,
            "fsGroup": SANDBOX_UID,
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        "initContainers": [init],
        "containers": [container],
        "volumes": [
            { "name": "tmp", "emptyDir": { "medium": "Memory", "sizeLimit": rec.limits.disk_bytes.to_string() } },
            { "name": "run", "emptyDir": { "medium": "Memory", "sizeLimit": "1Mi" } },
            { "name": "ruum-bin", "emptyDir": { "medium": "Memory", "sizeLimit": "16Mi" } },
            { "name": "ruum-id", "projected": {
                "defaultMode": 0o440,
                "sources": [{ "serviceAccountToken": {
                    "audience": cfg.supervisor_audience,
                    "expirationSeconds": IDENTITY_EXPIRY_SECS,
                    "path": "token",
                } }],
            } },
        ],
    });
    if !hosts.is_empty() {
        spec["hostAliases"] = json!(
            hosts
                .iter()
                .map(|h| json!({ "ip": h.ip, "hostnames": [h.alias] }))
                .collect::<Vec<_>>()
        );
    }
    if let KindRec::Job { timeout_ms } = &rec.kind {
        spec["activeDeadlineSeconds"] = json!(secs_ceil(*timeout_ms));
    }
    if let Some(nodes) = &cfg.node_selector {
        spec["nodeSelector"] = json!(nodes);
    }
    if !cfg.runtime_class_overhead.is_empty() {
        spec["overhead"] = json!(cfg.runtime_class_overhead);
    }
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": pod_name(id, rec.run), "labels": labels },
        "spec": spec,
    })
}

/// The pod the driver makes for a fixed sample instance under `cfg`: a service with a health
/// probe, a plain and a secret environment value, and every shared service of `cfg` (each at a
/// documentation address, as the driver would resolve it). The enable check of the Kubernetes
/// driver (review of I6-20, required change 2) sends it to the real API server as a server-side
/// dry run and holds the answer to [`check_stored_sample`], so the defaults that the API server
/// and its admission plugins add are known before a real pod is made.
pub fn sample_pod(cfg: &KubeConfig) -> Value {
    let routes: Vec<Route> = cfg.routes.keys().cloned().collect();
    let rec: Record = serde_json::from_value(json!({
        "v": 1,
        "instance": "0190f0f0-0000-7000-8000-00000000c0de",
        "identity": {
            "tenant": "0190f0f0-0000-7000-8000-000000000001",
            "app": "0190f0f0-0000-7000-8000-000000000002",
            "deployment": "0190f0f0-0000-7000-8000-000000000003",
            "environment": "live",
        },
        "kind": { "kind": "service", "port": 8080,
            "health": { "path": "/healthz", "interval_ms": 5000, "failure_threshold": 3 },
            "idle_timeout_ms": null },
        "routes": routes,
        "limits": { "cpu_millis": 250, "memory_bytes": 268435456, "disk_bytes": 67108864 },
        "image": format!("{}/ruum/sample@sha256:{}",
            cfg.registry.as_deref().unwrap_or("registry.invalid:5000"), "0".repeat(64)),
        "command": ["node", "server.js"],
        "env": [{ "name": "MODE", "value": "sample" }, { "name": "API_KEY" }],
        "started_at_ms": 1, "state": { "state": "starting" }, "run": 1,
        "run_started_ms": 1, "last_touch_ms": 1,
    }))
    .expect("the sample record parses");
    let hosts: Vec<HostAlias> = cfg
        .routes
        .values()
        .enumerate()
        .map(|(i, s)| HostAlias {
            ip: format!("192.0.2.{}", i + 10),
            alias: s.alias.clone(),
        })
        .collect();
    let scheme = if cfg.supervisor_tls { "https" } else { "http" };
    let launch = format!("{scheme}://192.0.2.1:{}", cfg.supervisor_port);
    pod_body(cfg, &rec, &hosts, &launch)
}

/// The read-back of the driver ([`verify`]) of `stored`, the API server's answer to
/// [`sample_pod`] of `cfg`. `Err` names the first setting that differs.
pub fn check_stored_sample(cfg: &KubeConfig, stored: &Value) -> Result<(), String> {
    verify(cfg, &sample_pod(cfg), stored)
}

/// A Kubernetes quantity in base units (bytes, or thousandths for `m`); `None` when it does not
/// parse. Covers the forms the API server writes back: plain integers, `m`, the decimal and the
/// binary suffixes.
pub(crate) fn quantity(q: &str) -> Option<u128> {
    let (num, mult): (&str, u128) = if let Some(n) = q.strip_suffix("Ki") {
        (n, 1 << 10)
    } else if let Some(n) = q.strip_suffix("Mi") {
        (n, 1 << 20)
    } else if let Some(n) = q.strip_suffix("Gi") {
        (n, 1 << 30)
    } else if let Some(n) = q.strip_suffix("Ti") {
        (n, 1 << 40)
    } else if let Some(n) = q.strip_suffix('k') {
        (n, 1_000)
    } else if let Some(n) = q.strip_suffix('M') {
        (n, 1_000_000)
    } else if let Some(n) = q.strip_suffix('G') {
        (n, 1_000_000_000)
    } else if let Some(n) = q.strip_suffix('T') {
        (n, 1_000_000_000_000)
    } else {
        (q, 1)
    };
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    num.parse::<u128>().ok()?.checked_mul(mult)
}

/// CPU in thousandths of a core (`250m`, `1`, `0.5` is not written back by the API server).
fn cpu_millis(q: &str) -> Option<u128> {
    match q.strip_suffix('m') {
        Some(m) => quantity(m),
        None => quantity(q)?.checked_mul(1000),
    }
}

/// Whether `path` holds a Kubernetes quantity (compared by value: the API server writes them in
/// its canonical form).
fn is_quantity(path: &str) -> bool {
    path.ends_with("/sizeLimit")
        || path.starts_with("/spec/overhead/")
        || [
            "/limits/cpu",
            "/limits/memory",
            "/requests/cpu",
            "/requests/memory",
        ]
        .iter()
        .any(|s| path.ends_with(s))
}

fn same_quantity(path: &str, a: &Value, b: &Value) -> bool {
    let q = |v: &Value| {
        let s = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        if path.ends_with("/cpu") {
            cpu_millis(&s)
        } else {
            quantity(&s)
        }
    };
    q(a).is_some() && q(a) == q(b)
}

/// The fields that the API server adds to a pod the driver sent (its defaults), with the values
/// it adds; nothing else may be in a stored pod that the driver did not send.
fn api_default(cfg: &KubeConfig, path: &str, v: &Value) -> bool {
    match path {
        "/spec/schedulerName" => v == &json!("default-scheduler"),
        // Set by the scheduler after the pod was stored.
        "/spec/nodeName" => v.is_string(),
        "/spec/serviceAccount" => v == &json!(cfg.sandbox_service_account),
        "/spec/priority" => v == &json!(0),
        "/spec/preemptionPolicy" => v == &json!("PreemptLowerPriority"),
        // The DefaultTolerationSeconds admission plugin: the not-ready and unreachable taints.
        "/spec/tolerations" => v.as_array().is_some_and(|a| {
            a.iter().all(|t| {
                matches!(
                    t.get("key").and_then(Value::as_str),
                    Some("node.kubernetes.io/not-ready" | "node.kubernetes.io/unreachable")
                ) && t.get("operator") == Some(&json!("Exists"))
                    && t.get("effect") == Some(&json!("NoExecute"))
                    && t.as_object().is_some_and(|o| {
                        o.keys().all(|k| {
                            matches!(
                                k.as_str(),
                                "key" | "operator" | "effect" | "tolerationSeconds"
                            )
                        })
                    })
            })
        }),
        _ if path.starts_with("/spec/containers/") || path.starts_with("/spec/initContainers/") => {
            path.ends_with("/protocol") && path.contains("/ports/") && v == &json!("TCP")
        }
        _ => false,
    }
}

/// The fields the API server omits when they are `false` and whose absence means `false` (its
/// default): only these may be missing from the stored object. `allowPrivilegeEscalation`,
/// `automountServiceAccountToken` or `enableServiceLinks` missing would mean `true`, so they
/// never are (Kimi review of I6-20, finding 5).
fn omittable(path: &str) -> bool {
    let key = path.rsplit('/').next().unwrap_or_default();
    matches!(
        key,
        "stdin"
            | "stdinOnce"
            | "tty"
            | "hostNetwork"
            | "hostPID"
            | "hostIPC"
            | "shareProcessNamespace"
            | "privileged"
    )
}

/// Go's zero values (with [`omittable`]: what may be missing).
fn zero(v: &Value) -> bool {
    match v {
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_i64() == Some(0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::Null => true,
    }
}

/// Compares what the API server stored (`got`) with what the driver sent, exhaustively: every
/// key of `got` must be one the driver sent (or an [`api_default`]), every value the driver sent
/// must be equal (quantities by value), and a sent key may be missing only with a zero value.
/// `Err` names the first path that differs.
fn compare(cfg: &KubeConfig, path: &str, sent: &Value, got: &Value) -> Result<(), String> {
    match (sent, got) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, gv) in b {
                let p = format!("{path}/{k}");
                match a.get(k) {
                    Some(sv) => compare(cfg, &p, sv, gv)?,
                    None if api_default(cfg, &p, gv) => {}
                    None => return Err(p),
                }
            }
            for (k, sv) in a {
                let p = format!("{path}/{k}");
                if !b.contains_key(k) && !(zero(sv) && omittable(&p)) {
                    return Err(p);
                }
            }
            Ok(())
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Err(path.to_owned());
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                compare(cfg, &format!("{path}/{i}"), x, y)?;
            }
            Ok(())
        }
        _ if is_quantity(path) => {
            if same_quantity(path, sent, got) {
                Ok(())
            } else {
                Err(path.to_owned())
            }
        }
        _ if sent == got => Ok(()),
        _ => Err(path.to_owned()),
    }
}

/// Reads the pod back as the API server stored it (`pod`) and checks it against the pod the
/// driver made for the record (`sent`): the whole spec exhaustively ([`compare`]: every key of
/// the stored spec must be one the driver sent or an API server default, and every value equal),
/// the labels exactly and no annotation (gVisor reads pod annotations). The server runs it after
/// each create and before it gives a pod its launch data (backlog S-89). `Err` names the first
/// setting that differs.
pub(crate) fn verify(cfg: &KubeConfig, sent: &Value, pod: &Value) -> Result<(), String> {
    let spec = pod.get("spec").ok_or("no spec")?;
    let want = sent.get("spec").ok_or("no spec sent")?;
    compare(cfg, "/spec", want, spec)?;
    if pod.pointer("/metadata/labels") != sent.pointer("/metadata/labels") {
        return Err("labels".into());
    }
    if pod
        .pointer("/metadata/annotations")
        .and_then(Value::as_object)
        .is_some_and(|a| !a.is_empty())
    {
        return Err("annotations".into());
    }
    if pod.pointer("/metadata/name") != sent.pointer("/metadata/name") {
        return Err("name".into());
    }
    // Belt and braces: the settings of rule 1 by name (the comparison already holds them).
    let s = |p: &str| spec.pointer(p);
    if s("/runtimeClassName") != Some(&json!(cfg.runtime_class))
        || s("/automountServiceAccountToken") != Some(&json!(false))
        || s("/serviceAccountName") != Some(&json!(cfg.sandbox_service_account))
        || spec
            .get("ephemeralContainers")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    {
        return Err("the sandbox settings".into());
    }
    Ok(())
}

/// What a pod says about its run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PodView {
    /// Not running yet. `broken`: it never will (a configuration or image name error).
    Pending {
        broken: bool,
    },
    Running {
        ip: Option<String>,
        ready: bool,
        started: Option<OffsetDateTime>,
    },
    Ended(ExitStatus),
    /// Being deleted (`metadata.deletionTimestamp`): it may still run through its grace time,
    /// but it is never running for the driver (no address for the gateway, no egress
    /// registration).
    Terminating,
}

pub(crate) fn view(pod: &Value) -> PodView {
    let phase = pod
        .pointer("/status/phase")
        .and_then(Value::as_str)
        .unwrap_or("Pending");
    if pod.pointer("/status/reason").and_then(Value::as_str) == Some("DeadlineExceeded") {
        return PodView::Ended(ExitStatus::of(ExitReason::TimedOut));
    }
    let cs = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find(|c| c.get("name").and_then(Value::as_str) == Some(CONTAINER))
        });
    if let Some(t) = cs.and_then(|c| c.pointer("/state/terminated")) {
        let reason = t.get("reason").and_then(Value::as_str).unwrap_or_default();
        if reason == "OOMKilled" {
            return PodView::Ended(ExitStatus::of(ExitReason::OutOfMemory));
        }
        return match t.get("exitCode").and_then(Value::as_i64) {
            Some(code) => PodView::Ended(ExitStatus::completed(code as i32)),
            None => PodView::Ended(ExitStatus::of(ExitReason::Killed)),
        };
    }
    match phase {
        "Succeeded" => return PodView::Ended(ExitStatus::completed(0)),
        "Failed" => return PodView::Ended(ExitStatus::of(ExitReason::Killed)),
        _ => {}
    }
    if pod.pointer("/metadata/deletionTimestamp").is_some() {
        return PodView::Terminating;
    }
    if let Some(r) = cs.and_then(|c| c.pointer("/state/running")) {
        let started = r.get("startedAt").and_then(Value::as_str).and_then(|s| {
            OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        });
        return PodView::Running {
            ip: pod
                .pointer("/status/podIP")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            ready: cs.and_then(|c| c.get("ready")).and_then(Value::as_bool) == Some(true),
            started,
        };
    }
    let waiting = cs
        .and_then(|c| c.pointer("/state/waiting/reason"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    PodView::Pending {
        broken: matches!(
            waiting,
            "CreateContainerConfigError"
                | "CreateContainerError"
                | "InvalidImageName"
                | "ErrImageNeverPull"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_parse_as_the_api_server_writes_them() {
        assert_eq!(quantity("67108864"), Some(64 << 20));
        assert_eq!(quantity("64Mi"), Some(64 << 20));
        assert_eq!(quantity("1Gi"), Some(1 << 30));
        assert_eq!(quantity("500k"), Some(500_000));
        assert_eq!(quantity("1.5Gi"), None);
        assert_eq!(quantity(""), None);
        assert_eq!(cpu_millis("250m"), Some(250));
        assert_eq!(cpu_millis("1"), Some(1000));
    }

    /// A record that a newer release wrote keeps its unknown fields when this release writes it
    /// back (design 12.7, decision 42: expand, then contract).
    #[test]
    fn a_record_keeps_fields_of_a_newer_release() {
        let id = InstanceId::new();
        let text = format!(
            r#"{{"v":1,"instance":"{}","identity":{{"tenant":"{}","app":"{}","deployment":"{}","environment":"live"}},
            "kind":{{"kind":"job","timeout_ms":1000}},"routes":[],"limits":{{"cpu_millis":250,"memory_bytes":1,"disk_bytes":1}},
            "image":"r/x@sha256:00","command":[],"env":[],"started_at_ms":1,"state":{{"state":"starting"}},"run":1,
            "run_started_ms":1,"last_touch_ms":1,"lease":{{"holder":"b","until_ms":9}}}}"#,
            id.0,
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7(),
            uuid::Uuid::now_v7()
        );
        let mut rec: Record = serde_json::from_str(&text).unwrap();
        assert_eq!(rec.extra["lease"]["holder"], "b");
        rec.run = 2;
        let back: Value = serde_json::to_value(&rec).unwrap();
        assert_eq!(back["lease"], json!({ "holder": "b", "until_ms": 9 }));
        assert_eq!(back["run"], 2);
    }

    #[test]
    fn a_pod_being_deleted_is_not_running() {
        let pod = json!({
            "metadata": { "deletionTimestamp": "2026-11-24T10:00:00Z" },
            "status": { "phase": "Running", "podIP": "10.0.0.9", "containerStatuses": [
                { "name": CONTAINER, "ready": true, "state": { "running": { "startedAt": "2026-11-24T09:00:00Z" } } }
            ] },
        });
        assert_eq!(view(&pod), PodView::Terminating);
    }

    #[test]
    fn names_fit_kubernetes() {
        let id = InstanceId::new();
        assert!(object_name(id).len() <= 63);
        assert!(pod_name(id, u32::MAX).len() <= 63);
    }

    fn record() -> Record {
        let mut rec: Record = serde_json::from_value(json!({
            "v": 1, "instance": InstanceId::new(),
            "identity": { "tenant": uuid::Uuid::now_v7(), "app": uuid::Uuid::now_v7(),
                "deployment": uuid::Uuid::now_v7(), "environment": "live" },
            "kind": { "kind": "service", "port": 8080, "health": null, "idle_timeout_ms": null },
            "routes": [], "limits": { "cpu_millis": 250, "memory_bytes": 67108864, "disk_bytes": 1048576 },
            "image": "r/ruum/t/a@sha256:00", "command": ["serve"],
            "env": [{ "name": "MODE", "value": "x" }, { "name": "API_KEY" }],
            "started_at_ms": 1, "state": { "state": "starting" }, "run": 1,
            "run_started_ms": 1, "last_touch_ms": 1,
        }))
        .unwrap();
        rec.run = 1;
        rec
    }

    fn config() -> KubeConfig {
        let mut c = KubeConfig::new("https://k", "ruum-apps", "gvisor");
        c.supervisor_image = "r/ruum-supervisor@sha256:11".into();
        c
    }

    /// The exhaustive read-back (S-89): API server defaults pass; any other added or changed key
    /// fails, also deep in a container, a volume or a mount.
    #[test]
    fn the_read_back_is_exhaustive() {
        let cfg = config();
        let rec = record();
        let sent = pod_body(&cfg, &rec, &[], "http://10.0.0.5:8091");
        assert_eq!(verify(&cfg, &sent, &sent), Ok(()));
        // The defaults of a real API server, and quantities in canonical form.
        let mut stored = sent.clone();
        stored["spec"]["schedulerName"] = json!("default-scheduler");
        stored["spec"]["serviceAccount"] = json!("ruum-sandbox");
        stored["spec"]["priority"] = json!(0);
        stored["spec"]["nodeName"] = json!("node-1");
        stored["spec"]["tolerations"] = json!([{ "key": "node.kubernetes.io/not-ready",
            "operator": "Exists", "effect": "NoExecute", "tolerationSeconds": 300 }]);
        stored["spec"]["containers"][0]["ports"][0]["protocol"] = json!("TCP");
        stored["spec"]["containers"][0]["resources"]["limits"]["memory"] = json!("64Mi");
        stored["spec"]["volumes"][0]["emptyDir"]["sizeLimit"] = json!("1Mi");
        stored["spec"]["containers"][0]
            .as_object_mut()
            .unwrap()
            .remove("stdin");
        assert_eq!(verify(&cfg, &sent, &stored), Ok(()));
        // Each of these must be refused.
        type Change = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Change)> = vec![
            (
                "a capability",
                Box::new(|p| {
                    p["spec"]["containers"][0]["securityContext"]["capabilities"]["add"] =
                        json!(["NET_RAW"])
                }),
            ),
            (
                "a writable supervisor",
                Box::new(|p| {
                    p["spec"]["containers"][0]["volumeMounts"][2]["readOnly"] = json!(false)
                }),
            ),
            (
                "a mount option",
                Box::new(|p| p["spec"]["containers"][0]["volumeMounts"][1]["subPath"] = json!("x")),
            ),
            (
                "another init image",
                Box::new(|p| p["spec"]["initContainers"][0]["image"] = json!("evil@sha256:22")),
            ),
            (
                "another audience",
                Box::new(|p| {
                    p["spec"]["volumes"][3]["projected"]["sources"][0]["serviceAccountToken"]["audience"] =
                        json!("kubernetes")
                }),
            ),
            (
                "a longer token",
                Box::new(|p| {
                    p["spec"]["volumes"][3]["projected"]["sources"][0]["serviceAccountToken"]["expirationSeconds"] =
                        json!(86400)
                }),
            ),
            (
                "a workingDir",
                Box::new(|p| p["spec"]["containers"][0]["workingDir"] = json!("/etc")),
            ),
            (
                "a lifecycle hook",
                Box::new(|p| {
                    p["spec"]["containers"][0]["lifecycle"] =
                        json!({ "preStop": { "exec": { "command": ["sh"] } } })
                }),
            ),
            (
                "another termination path",
                Box::new(|p| {
                    p["spec"]["containers"][0]["terminationMessagePath"] = json!("/etc/passwd")
                }),
            ),
            (
                "a fallback policy",
                Box::new(|p| {
                    p["spec"]["containers"][0]["terminationMessagePolicy"] =
                        json!("FallbackToLogsOnError")
                }),
            ),
            (
                "another command",
                Box::new(|p| {
                    p["spec"]["containers"][0]["command"][3] = json!("http://10.9.9.9:8091")
                }),
            ),
            (
                "a host volume",
                Box::new(|p| {
                    p["spec"]["volumes"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({ "name": "h", "hostPath": { "path": "/" } }))
                }),
            ),
            (
                "a toleration",
                Box::new(
                    |p| p["spec"]["tolerations"] = json!([{ "key": "node-role.kubernetes.io/control-plane", "operator": "Exists", "effect": "NoSchedule" }]),
                ),
            ),
            (
                "a priority class",
                Box::new(|p| p["spec"]["priorityClassName"] = json!("system-node-critical")),
            ),
            (
                "an annotation",
                Box::new(|p| {
                    p["metadata"]["annotations"] = json!({ "dev.gvisor.internal.nvproxy": "true" })
                }),
            ),
            (
                "another label",
                Box::new(|p| p["metadata"]["labels"]["app"] = json!("ruum-gateway")),
            ),
            (
                "an ephemeral container",
                Box::new(|p| p["spec"]["ephemeralContainers"] = json!([{ "name": "x" }])),
            ),
            (
                "a host port",
                Box::new(|p| p["spec"]["containers"][0]["ports"][0]["hostPort"] = json!(80)),
            ),
            (
                "an env value from elsewhere",
                Box::new(
                    |p| p["spec"]["containers"][0]["env"][0] = json!({ "name": "MODE", "valueFrom": { "secretKeyRef": { "name": "x", "key": "y" } } }),
                ),
            ),
            (
                "no allowPrivilegeEscalation",
                Box::new(|p| {
                    p["spec"]["containers"][0]["securityContext"]
                        .as_object_mut()
                        .unwrap()
                        .remove("allowPrivilegeEscalation");
                }),
            ),
            (
                "no automountServiceAccountToken",
                Box::new(|p| {
                    p["spec"]
                        .as_object_mut()
                        .unwrap()
                        .remove("automountServiceAccountToken");
                }),
            ),
            (
                "no enableServiceLinks",
                Box::new(|p| {
                    p["spec"]
                        .as_object_mut()
                        .unwrap()
                        .remove("enableServiceLinks");
                }),
            ),
        ];
        for (what, change) in cases {
            let mut p = stored.clone();
            change(&mut p);
            assert!(verify(&cfg, &sent, &p).is_err(), "{what} was accepted");
        }
    }

    /// The runtime class's overhead (review of I6-20, required change 1): the RuntimeClass
    /// admission plugin writes it into every pod of the class. The driver sends the configured
    /// value, so a stored pod with that value (in the API server's form) passes, and one with
    /// another value, an added resource or an overhead the driver did not send fails.
    #[test]
    fn the_runtime_class_overhead_must_be_the_configured_one() {
        let mut cfg = config();
        cfg.runtime_class_overhead = BTreeMap::from([
            ("cpu".into(), "50m".into()),
            ("memory".into(), "64Mi".into()),
        ]);
        let rec = record();
        let sent = pod_body(&cfg, &rec, &[], "http://10.0.0.5:8091");
        assert_eq!(
            sent["spec"]["overhead"],
            json!({ "cpu": "50m", "memory": "64Mi" })
        );
        let mut stored = sent.clone();
        stored["spec"]["overhead"] = json!({ "cpu": "50m", "memory": "67108864" });
        assert_eq!(verify(&cfg, &sent, &stored), Ok(()));
        for other in [
            json!({ "cpu": "50m", "memory": "128Mi" }),
            json!({ "cpu": "1", "memory": "64Mi" }),
            json!({ "cpu": "50m", "memory": "64Mi", "ephemeral-storage": "1Gi" }),
            json!({ "cpu": "50m" }),
        ] {
            let mut p = sent.clone();
            p["spec"]["overhead"] = other.clone();
            assert!(verify(&cfg, &sent, &p).is_err(), "{other} was accepted");
        }
        // A driver that knows no overhead refuses a pod that has one (the class has one the
        // setting does not name): every start fails closed, never with an unchecked field.
        let plain = config();
        let sent = pod_body(&plain, &rec, &[], "http://10.0.0.5:8091");
        assert!(sent["spec"].get("overhead").is_none());
        let mut p = sent.clone();
        p["spec"]["overhead"] = json!({ "cpu": "50m", "memory": "64Mi" });
        assert_eq!(verify(&plain, &sent, &p), Err("/spec/overhead".into()));
        // The setting is checked.
        let mut bad = config();
        let overhead_refused = |c: &KubeConfig| matches!(c.check(), Err(ruum_core::runtime::RuntimeError::Internal { reason }) if reason.contains("overhead"));
        assert!(!overhead_refused(&cfg));
        bad.runtime_class_overhead = BTreeMap::from([("gpu".into(), "1".into())]);
        assert!(overhead_refused(&bad));
        bad.runtime_class_overhead = BTreeMap::from([("memory".into(), "lots".into())]);
        assert!(overhead_refused(&bad));
    }

    /// The pod holds no secret value, takes values from no Secret, and the digest of the fixed
    /// fields changes with each of them.
    #[test]
    fn the_pod_holds_no_secret_and_the_digest_covers_the_fixed_fields() {
        let cfg = config();
        let rec = record();
        let pod = pod_body(&cfg, &rec, &[], "http://10.0.0.5:8091");
        let text = pod.to_string();
        assert!(
            !text.contains("secretKeyRef") && !text.contains("\"secret\""),
            "{text}"
        );
        assert!(
            !text.contains("API_KEY"),
            "a secret name in the pod: {text}"
        );
        let d = fixed_digest(&rec);
        for change in [
            |r: &mut Record| r.image = "r/ruum/t/a@sha256:ff".into(),
            |r: &mut Record| r.command = vec!["other".into()],
            |r: &mut Record| r.env[0].value = Some("y".into()),
            |r: &mut Record| r.limits.memory_bytes += 1,
            |r: &mut Record| r.routes = vec![Route::EgressProxy],
        ] {
            let mut r = rec.clone();
            change(&mut r);
            assert_ne!(fixed_digest(&r), d);
        }
        let mut r = rec.clone();
        r.state = InstanceState::Running;
        r.run = 7;
        r.last_touch_ms = 99;
        assert_eq!(fixed_digest(&r), d, "a state change is not a fixed field");
    }
}
