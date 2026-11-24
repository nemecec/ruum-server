//! The Kubernetes objects of one instance: the record (a ConfigMap), the pod of each run and
//! what the driver reads back from them.
//!
//! Per instance `ruum-i-<id>` (the ID without dashes):
//!
//! - **ConfigMap `ruum-i-<id>`**, key `record`: the [`Record`] (identity, kind, routes, limits,
//!   image, command, plain environment, state, run). It is the instance's state, shared by every
//!   server process: any of them answers `status`, `stop`, `wake` and `touch`, also after a
//!   restart (adoption, backlog R2A-1). State changes replace it with its `resourceVersion`, so
//!   of two processes only one makes a change (and its effects: usage, unregistration).
//! - **Secret `ruum-i-<id>`**: the workload token (key `token`) and the secret environment values
//!   (keys `env.<NAME>`). The driver writes it and never reads it (its Role has no `get` on
//!   Secrets); the pod mounts the token as a file and takes the values as environment values.
//! - **Pod `ruum-i-<id>-<run>`**: one per run (a cold start makes a new pod).

use super::KubeConfig;
use crate::driver::{LABEL_APP, LABEL_DEPLOYMENT, LABEL_ENV, LABEL_INSTANCE, LABEL_TENANT};
use ruum_core::runtime::{
    EnvValue, ExitReason, ExitStatus, InstanceId, InstanceState, Limits, Route, TOKEN_FILE,
    TOKEN_FILE_ENV, WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use time::OffsetDateTime;

/// The label of the run on a pod.
pub const LABEL_RUN: &str = "ruum.run";
/// The container name in every pod.
pub(crate) const CONTAINER: &str = "app";
/// The user and group every sandbox runs as (the image's `USER` is ignored).
pub(crate) const SANDBOX_UID: i64 = 10001;

/// The name of an instance's ConfigMap and Secret.
pub fn object_name(id: InstanceId) -> String {
    format!("ruum-i-{}", id.0.simple())
}

/// The name of the pod of one run.
pub fn pod_name(id: InstanceId, run: u32) -> String {
    format!("{}-{run}", object_name(id))
}

/// The Secret key of a secret environment value.
pub(crate) fn env_key(name: &str) -> String {
    format!("env.{name}")
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

/// One environment variable: a plain value, or (`value: None`) a key of the instance's Secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EnvRec {
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// The state of an instance, as the ConfigMap holds it. No secret value is in it.
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
        }
    }
}

/// The labels of every object of an instance (the record, the Secret, each pod).
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

/// The Secret of an instance: the token and the secret environment values.
pub(crate) fn secret_body(cfg: &KubeConfig, spec: &WorkloadSpec, id: InstanceId) -> Value {
    let mut data = serde_json::Map::new();
    data.insert(
        "token".to_owned(),
        json!(spec.workload_token.expose().to_owned()),
    );
    for v in &spec.env {
        if let EnvValue::Secret(s) = &v.value {
            data.insert(env_key(&v.name), json!(s.expose().to_owned()));
        }
    }
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "Opaque",
        "metadata": { "name": object_name(id), "labels": labels(cfg, &spec.identity, id) },
        "stringData": Value::Object(data),
    })
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

/// The mount folder and the file name of the token.
fn token_mount() -> (&'static str, &'static str) {
    TOKEN_FILE
        .rsplit_once('/')
        .unwrap_or(("/var/run/ruum", "token"))
}

/// The pod of one run: the sandbox of STEP3 "How an app runs" rule 1 as a restricted pod in
/// gVisor (see the crate README "Kubernetes driver").
pub(crate) fn pod_body(cfg: &KubeConfig, rec: &Record, hosts: &[HostAlias]) -> Value {
    let id = rec.instance;
    let secret = object_name(id);
    let mut env: Vec<Value> = rec
        .env
        .iter()
        .map(|e| match &e.value {
            Some(v) => json!({ "name": e.name, "value": v }),
            None => json!({
                "name": e.name,
                "valueFrom": { "secretKeyRef": { "name": secret, "key": env_key(&e.name) } },
            }),
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
    let (mount, file) = token_mount();
    let mut container = json!({
        "name": CONTAINER,
        "image": rec.image,
        "imagePullPolicy": "IfNotPresent",
        "env": env,
        "resources": {
            "limits": { "cpu": format!("{}m", rec.limits.cpu_millis), "memory": rec.limits.memory_bytes.to_string() },
            "requests": { "cpu": format!("{}m", rec.limits.cpu_millis), "memory": rec.limits.memory_bytes.to_string() },
        },
        "securityContext": {
            "allowPrivilegeEscalation": false,
            "privileged": false,
            "readOnlyRootFilesystem": true,
            "runAsNonRoot": true,
            "runAsUser": SANDBOX_UID,
            "runAsGroup": SANDBOX_UID,
            "capabilities": { "drop": ["ALL"] },
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        "volumeMounts": [
            { "name": "tmp", "mountPath": "/tmp" },
            { "name": "token", "mountPath": mount, "readOnly": true },
        ],
        "stdin": false,
        "tty": false,
        "terminationMessagePolicy": "File",
    });
    if !rec.command.is_empty() {
        container["args"] = json!(rec.command);
    }
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
    let mut spec = json!({
        "runtimeClassName": cfg.runtime_class,
        "automountServiceAccountToken": false,
        "enableServiceLinks": false,
        "hostNetwork": false,
        "hostPID": false,
        "hostIPC": false,
        "shareProcessNamespace": false,
        "restartPolicy": "Never",
        "terminationGracePeriodSeconds": cfg.stop_timeout.as_secs(),
        // No DNS: the sandbox finds the shared services by /etc/hosts (the network policy lets it
        // reach only the egress proxy, which resolves names itself).
        "dnsPolicy": "None",
        "dnsConfig": { "nameservers": ["127.0.0.1"] },
        "securityContext": {
            "runAsNonRoot": true,
            "runAsUser": SANDBOX_UID,
            "runAsGroup": SANDBOX_UID,
            "fsGroup": SANDBOX_UID,
            "seccompProfile": { "type": "RuntimeDefault" },
        },
        "containers": [container],
        "volumes": [
            { "name": "tmp", "emptyDir": { "medium": "Memory", "sizeLimit": rec.limits.disk_bytes.to_string() } },
            { "name": "token", "secret": {
                "secretName": secret,
                "items": [{ "key": "token", "path": file }],
                "defaultMode": 0o440,
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
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": { "name": pod_name(id, rec.run), "labels": labels },
        "spec": spec,
    })
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

/// Reads the pod back as the API server stored it and checks every sandbox setting: a pod that
/// an admission step changed is refused (the driver deletes it). `Err` names the first setting
/// that differs.
pub(crate) fn verify(cfg: &KubeConfig, rec: &Record, pod: &Value) -> Result<(), String> {
    let spec = pod.get("spec").ok_or("no spec")?;
    let s = |p: &str| spec.pointer(p);
    let is = |p: &str, want: &Value| s(p) == Some(want);
    let not_true = |p: &str| s(p).and_then(Value::as_bool) != Some(true);
    if !is("/runtimeClassName", &json!(cfg.runtime_class)) {
        return Err("runtimeClassName".into());
    }
    if !is("/automountServiceAccountToken", &json!(false)) {
        return Err("automountServiceAccountToken".into());
    }
    for p in [
        "/hostNetwork",
        "/hostPID",
        "/hostIPC",
        "/shareProcessNamespace",
    ] {
        if !not_true(p) {
            return Err(p[1..].to_owned());
        }
    }
    if !is("/restartPolicy", &json!("Never")) {
        return Err("restartPolicy".into());
    }
    if !is("/securityContext/runAsNonRoot", &json!(true))
        || !is("/securityContext/runAsUser", &json!(SANDBOX_UID))
    {
        return Err("the pod's securityContext".into());
    }
    if spec
        .get("initContainers")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
        || spec
            .get("ephemeralContainers")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    {
        return Err("other containers".into());
    }
    let containers = spec
        .get("containers")
        .and_then(Value::as_array)
        .ok_or("no containers")?;
    let [c] = containers.as_slice() else {
        return Err("not exactly one container".into());
    };
    let c_is = |p: &str, want: &Value| c.pointer(p) == Some(want);
    if !c_is("/image", &json!(rec.image)) {
        return Err("image".into());
    }
    if !c_is("/securityContext/allowPrivilegeEscalation", &json!(false))
        || !c_is("/securityContext/readOnlyRootFilesystem", &json!(true))
        || c.pointer("/securityContext/privileged")
            .and_then(Value::as_bool)
            == Some(true)
        || !c_is("/securityContext/capabilities/drop", &json!(["ALL"]))
        || c.pointer("/securityContext/capabilities/add")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    {
        return Err("the container's securityContext".into());
    }
    for kind in ["limits", "requests"] {
        let cpu = c
            .pointer(&format!("/resources/{kind}/cpu"))
            .and_then(Value::as_str)
            .and_then(cpu_millis);
        let mem = c
            .pointer(&format!("/resources/{kind}/memory"))
            .and_then(Value::as_str)
            .and_then(quantity);
        if cpu != Some(u128::from(rec.limits.cpu_millis))
            || mem != Some(u128::from(rec.limits.memory_bytes))
        {
            return Err(format!("resources.{kind}"));
        }
    }
    let volumes = spec
        .get("volumes")
        .and_then(Value::as_array)
        .ok_or("no volumes")?;
    if volumes.len() != 2 {
        return Err("volumes".into());
    }
    for v in volumes {
        match v.get("name").and_then(Value::as_str) {
            Some("tmp") => {
                let size = v
                    .pointer("/emptyDir/sizeLimit")
                    .and_then(Value::as_str)
                    .and_then(quantity);
                if v.pointer("/emptyDir/medium") != Some(&json!("Memory"))
                    || size != Some(u128::from(rec.limits.disk_bytes))
                {
                    return Err("the tmp volume".into());
                }
            }
            Some("token") => {
                if v.pointer("/secret/secretName") != Some(&json!(object_name(rec.instance))) {
                    return Err("the token volume".into());
                }
            }
            _ => return Err("an unknown volume".into()),
        }
    }
    let mounts = c
        .get("volumeMounts")
        .and_then(Value::as_array)
        .ok_or("no volume mounts")?;
    let (mount, _) = token_mount();
    let ok = mounts.len() == 2
        && mounts.iter().all(|m| {
            match (
                m.get("name").and_then(Value::as_str),
                m.get("mountPath").and_then(Value::as_str),
            ) {
                (Some("tmp"), Some("/tmp")) => true,
                (Some("token"), Some(p)) => {
                    p == mount && m.get("readOnly").and_then(Value::as_bool) == Some(true)
                }
                _ => false,
            }
        });
    if !ok {
        return Err("volume mounts".into());
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

    #[test]
    fn names_fit_kubernetes() {
        let id = InstanceId::new();
        assert!(object_name(id).len() <= 63);
        assert!(pod_name(id, u32::MAX).len() <= 63);
    }
}
