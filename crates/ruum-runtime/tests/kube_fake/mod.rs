//! A fake Kubernetes API server with a fake kubelet, for the Kubernetes driver's tests (I6-1).
//!
//! - **The API**: pods, ConfigMaps and Secrets of namespaces (each with a `uid`), pod logs
//!   (followed, one stream or both), label selectors, resourceVersion conflicts on `PUT`, merge
//!   patches with `stringData`, and `TokenReview`s of the projected tokens the fake kubelet
//!   issued (audience, ServiceAccount, the pod's name and uid, `exp`). A bearer token is
//!   required. As a real API server, a Secret `patch` answers with the changed Secret and a
//!   Secret `delete` with `propagationPolicy: Foreground` or `Orphan` answers with the Secret.
//! - **The garbage collector**: deleting an object deletes every object whose `ownerReferences`
//!   name its uid, and theirs (at once, or after `gc_delay_ms`).
//! - **RBAC**: the rules of the driver's Role and ClusterRole are read from the chart
//!   (`deploy/staging/charts/platform/templates/runtime-rbac.yaml`) and every other verb,
//!   resource or namespace is `403`. So a driver change that needs a new right fails here until
//!   the chart has it.
//! - **Admission**: the namespace's rules as `k8s/manifests/apps-admission.yaml` states them
//!   ([`admit`], [`admit_secret`]); quantities are written back in the API server's canonical
//!   form (`64Mi`), as a real API server does.
//! - **The kubelet and the sandbox supervisor** (backlog I6-20, S-89): a pod with a projected
//!   service account token gets a token the fake issued for that pod (its name and uid). A pod
//!   whose command is the supervisor's fetches its launch data from the `--launch-url` (the
//!   test's supervisor listener) with that token and its address (header `x-fake-peer`), then
//!   runs the conformance program of the launch command (`/ruum-program` and its steps,
//!   `ruum_core::runtime::program`), redacting every line with the launch values through
//!   `ruum-core`'s redactor as the real supervisor does, before the line reaches the pod log. A
//!   token step asks `/v1/token` for the current token first. A refused launch ends the pod with
//!   125 ([`Fake::launches`] records each answer). Also: the memory limit (`OOMKilled`), the size
//!   of the memory-backed `/tmp` (a full disk), `activeDeadlineSeconds` (`DeadlineExceeded`) and
//!   the readiness of `serve` and `unhealthy`.
//! - **The registry**: manifests (`HEAD` and `GET`) and config blobs of pushed images only.

#![allow(dead_code)]

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ruum_core::runtime::program::{DISK_FULL, Step, token_sha};
use ruum_core::runtime::redact::LineRedactor;
use ruum_supervisor::protocol::{SUPERVISOR_PATH, TokenAnswer};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

pub const TOKEN: &str = "fake-api-token-of-the-server";

/// The output of one pod: chunks in order, each with its stream (1 stdout, 2 stderr).
#[derive(Default)]
pub struct LogBuf {
    chunks: Mutex<Vec<(u8, Bytes)>>,
    ended: AtomicBool,
    notify: Notify,
}

impl LogBuf {
    fn push(&self, stream: u8, data: Vec<u8>) {
        self.chunks
            .lock()
            .unwrap()
            .push((stream, Bytes::from(data)));
        self.notify.notify_waiters();
    }
    fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

struct PodState {
    obj: Value,
    log: Arc<LogBuf>,
    task: Option<JoinHandle<()>>,
    started: bool,
}

#[derive(Default)]
struct Objects {
    rv: u64,
    pods: BTreeMap<(String, String), PodState>,
    cms: BTreeMap<(String, String), Value>,
    secrets: BTreeMap<(String, String), Value>,
}

/// How the fake changes a stored pod (to test the driver's read-back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tamper {
    None,
    AddCapability,
    DropRuntimeClass,
    /// An environment value from another instance's Secret.
    InjectSecretEnv,
    /// Another `/etc/hosts` line.
    ChangeHostAlias,
    /// Another service account.
    ServiceAccount,
    /// A host port on the container.
    HostPort,
    /// A writable supervisor volume in the app container.
    WritableSupervisor,
    /// Another audience of the projected token.
    TokenAudience,
}

/// What the fake knows of a projected token it issued.
#[derive(Clone, Debug)]
pub struct Issued {
    pub audience: String,
    pub namespace: String,
    pub service_account: String,
    pub pod: String,
    pub pod_uid: String,
    pub exp: i64,
}

pub struct Fake {
    objs: Mutex<Objects>,
    rules: Vec<(String, Vec<String>)>,
    cluster_rules: Vec<(String, Vec<String>)>,
    /// The projected tokens the kubelet issued.
    pub issued: Mutex<HashMap<String, Issued>>,
    /// Each launch answer the supervisor got: (pod, HTTP status).
    pub launches: Mutex<Vec<(String, u16)>>,
    /// The next pod's supervisor does not ask for its launch (the test asks itself).
    pub skip_launch_once: AtomicBool,
    /// The registry's objects by digest (manifests and config blobs).
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    namespace: String,
    pub calls: Mutex<Vec<(String, String, String)>>,
    registry: Mutex<HashSet<(String, String)>>,
    pub tamper: Mutex<Tamper>,
    next_ip: AtomicU32,
    /// The next ConfigMap list answers empty (a start that the sweep's first list missed).
    pub hide_records_once: AtomicBool,
    /// The next log request answers "waiting to start" after 300 ms, whatever the pod does (a
    /// short job that ended between the log request and the pod look).
    pub log_not_ready_once: AtomicBool,
    /// How long the garbage collector waits before it deletes the dependents of a deleted
    /// object (0: at once).
    pub gc_delay_ms: AtomicU64,
}

/// The Role of the driver, read from the chart: `(resource, verbs)`.
pub fn role_rules() -> Vec<(String, Vec<String>)> {
    rules_of("kind: Role", "[\"\"]")
}

/// The ClusterRole of the driver (the `TokenReview`s of backlog S-89): `(resource, verbs)`.
pub fn cluster_rules() -> Vec<(String, Vec<String>)> {
    rules_of("kind: ClusterRole", "[\"authentication.k8s.io\"]")
}

fn rules_of(kind_line: &str, group: &str) -> Vec<(String, Vec<String>)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/staging/charts/platform/templates/runtime-rbac.yaml"
    );
    let text = std::fs::read_to_string(path).expect("the driver's Role in the chart");
    let mut out = Vec::new();
    let mut in_role = false;
    for line in text.lines() {
        if line.starts_with("kind: ") {
            in_role = line == kind_line;
        }
        let l = line.trim();
        if !in_role || !l.starts_with(&format!("- {{ apiGroups: {group}, resources: [")) {
            continue;
        }
        let res = l
            .split("resources: [")
            .nth(1)
            .and_then(|r| r.split(']').next())
            .unwrap();
        let verbs = l
            .split("verbs: [")
            .nth(1)
            .and_then(|r| r.split(']').next())
            .unwrap();
        for r in res.split(',') {
            out.push((
                r.trim().to_owned(),
                verbs.split(',').map(|v| v.trim().to_owned()).collect(),
            ));
        }
    }
    assert!(!out.is_empty(), "no {kind_line} rules read from {path}");
    out
}

/// The API server's form of a byte quantity: `64Mi` when it divides, the number otherwise.
fn canonical_bytes(q: &str) -> String {
    match q.parse::<u64>() {
        Ok(n) if n > 0 && n % (1 << 30) == 0 => format!("{}Gi", n >> 30),
        Ok(n) if n > 0 && n % (1 << 20) == 0 => format!("{}Mi", n >> 20),
        Ok(n) if n > 0 && n % (1 << 10) == 0 => format!("{}Ki", n >> 10),
        _ => q.to_owned(),
    }
}

pub fn bytes_of(q: &str) -> u64 {
    for (suffix, mult) in [("Gi", 1u64 << 30), ("Mi", 1 << 20), ("Ki", 1 << 10)] {
        if let Some(n) = q.strip_suffix(suffix) {
            return n.parse::<u64>().unwrap() * mult;
        }
    }
    q.parse().unwrap()
}

fn status(code: StatusCode, message: &str) -> Response {
    (
        code,
        axum::Json(json!({ "kind": "Status", "status": "Failure", "message": message, "code": code.as_u16() })),
    )
        .into_response()
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

fn matches_selector(obj: &Value, selector: Option<&String>) -> bool {
    let Some(sel) = selector else { return true };
    sel.split(',').filter(|s| !s.is_empty()).all(|pair| {
        let (k, v) = pair.split_once('=').unwrap();
        obj.pointer("/metadata/labels")
            .and_then(|l| l.get(k))
            .and_then(Value::as_str)
            == Some(v)
    })
}

fn merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge(t.entry(k.clone()).or_insert(Value::Null), v);
                }
            }
        }
        (t, p) => *t = p.clone(),
    }
}

impl Fake {
    pub async fn start(namespace: &str) -> (Arc<Fake>, String) {
        let fake = Arc::new(Fake {
            objs: Mutex::new(Objects::default()),
            rules: role_rules(),
            cluster_rules: cluster_rules(),
            issued: Mutex::new(HashMap::new()),
            launches: Mutex::new(Vec::new()),
            skip_launch_once: AtomicBool::new(false),
            blobs: Mutex::new(HashMap::new()),
            namespace: namespace.to_owned(),
            calls: Mutex::new(Vec::new()),
            registry: Mutex::new(HashSet::new()),
            tamper: Mutex::new(Tamper::None),
            next_ip: AtomicU32::new(2),
            hide_records_once: AtomicBool::new(false),
            log_not_ready_once: AtomicBool::new(false),
            gc_delay_ms: AtomicU64::new(0),
        });
        let app = Router::new()
            .route(
                "/api/v1/namespaces/{ns}/{kind}",
                get(collection_get).post(collection_post),
            )
            .route(
                "/api/v1/namespaces/{ns}/{kind}/{name}",
                get(object_get)
                    .put(object_put)
                    .patch(object_patch)
                    .delete(object_delete),
            )
            .route("/api/v1/namespaces/{ns}/pods/{name}/log", get(pod_log))
            .route(
                "/apis/authentication.k8s.io/v1/tokenreviews",
                axum::routing::post(token_review),
            )
            .route("/v2/{*rest}", get(registry))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (fake, url)
    }

    /// Makes an image known to the registry.
    pub fn push(&self, repo: &str, digest: &str) {
        self.registry
            .lock()
            .unwrap()
            .insert((repo.to_owned(), digest.to_owned()));
    }

    /// Pushes an image manifest and its config blob; returns the manifest's digest.
    pub fn push_image(&self, repo: &str, manifest: &[u8], config: &[u8]) -> String {
        let digest = ruum_core::runtime::OciDigest::of(manifest)
            .as_str()
            .to_owned();
        let cfg = ruum_core::runtime::OciDigest::of(config)
            .as_str()
            .to_owned();
        let mut b = self.blobs.lock().unwrap();
        b.insert(digest.clone(), manifest.to_vec());
        b.insert(cfg.clone(), config.to_vec());
        drop(b);
        self.push(repo, &digest);
        self.push(repo, &cfg);
        digest
    }

    /// Issues a projected token for a pod (what the kubelet does for a `serviceAccountToken`
    /// volume source). The token is a JWT in shape, with `exp`.
    pub fn issue(&self, i: Issued) -> String {
        use base64::Engine;
        let b64 = |v: &Value| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
        };
        let token = format!(
            "{}.{}.{}",
            b64(&json!({ "alg": "RS256" })),
            b64(
                &json!({ "aud": [i.audience], "exp": i.exp, "sub": format!("system:serviceaccount:{}:{}", i.namespace, i.service_account), "jti": uuid::Uuid::now_v7().to_string() })
            ),
            "fake-signature"
        );
        self.issued.lock().unwrap().insert(token.clone(), i);
        token
    }

    /// `Err` (401 or 403) unless the token and the Role allow the call.
    #[allow(clippy::result_large_err)]
    fn allow(
        &self,
        headers: &HeaderMap,
        ns: &str,
        verb: &str,
        resource: &str,
    ) -> Result<(), Response> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if auth != format!("Bearer {TOKEN}") {
            return Err(status(StatusCode::UNAUTHORIZED, "Unauthorized"));
        }
        self.calls
            .lock()
            .unwrap()
            .push((ns.to_owned(), verb.to_owned(), resource.to_owned()));
        let rules = if ns.is_empty() {
            &self.cluster_rules
        } else {
            &self.rules
        };
        let ok = (ns.is_empty() || ns == self.namespace)
            && rules
                .iter()
                .any(|(r, verbs)| r == resource && verbs.iter().any(|v| v == verb));
        if ok {
            Ok(())
        } else {
            Err(status(
                StatusCode::FORBIDDEN,
                &format!("{verb} {resource} in {ns} is forbidden"),
            ))
        }
    }

    pub fn pod(&self, ns: &str, name: &str) -> Option<Value> {
        self.objs
            .lock()
            .unwrap()
            .pods
            .get(&(ns.to_owned(), name.to_owned()))
            .map(|p| p.obj.clone())
    }

    /// Everything a pod wrote to its log, both streams (what the node's CRI log file holds).
    pub fn raw_log(&self, ns: &str, name: &str) -> String {
        let o = self.objs.lock().unwrap();
        let Some(p) = o.pods.get(&(ns.to_owned(), name.to_owned())) else {
            return String::new();
        };
        let chunks = p.log.chunks.lock().unwrap();
        chunks
            .iter()
            .map(|(_, b)| String::from_utf8_lossy(b).into_owned())
            .collect()
    }

    /// Moves the start of a pod's app container `secs` into the past (as the kubelet reports it).
    pub fn age_pod(&self, ns: &str, name: &str, secs: i64) {
        let t = (time::OffsetDateTime::now_utc() - time::Duration::seconds(secs))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        self.set_pod(&(ns.to_owned(), name.to_owned()), |p| {
            p["status"]["containerStatuses"][0]["state"]["running"]["startedAt"] = json!(t);
        });
    }

    pub fn pods(&self) -> Vec<Value> {
        let o = self.objs.lock().unwrap();
        o.pods.values().map(|p| p.obj.clone()).collect()
    }

    pub fn config_maps(&self) -> Vec<Value> {
        self.objs.lock().unwrap().cms.values().cloned().collect()
    }

    pub fn secret_names(&self) -> Vec<String> {
        self.objs
            .lock()
            .unwrap()
            .secrets
            .keys()
            .map(|(_, n)| n.clone())
            .collect()
    }

    /// Deletes a pod as a node loss or an operator would.
    pub fn remove_pod(&self, ns: &str, name: &str) {
        let p = self
            .objs
            .lock()
            .unwrap()
            .pods
            .remove(&(ns.to_owned(), name.to_owned()));
        if let Some(mut p) = p {
            if let Some(t) = p.task.take() {
                t.abort();
            }
            p.log.end();
        }
    }

    fn bump(o: &mut Objects, obj: &mut Value) {
        o.rv += 1;
        obj["metadata"]["resourceVersion"] = json!(o.rv.to_string());
    }

    /// Changes the stored pod's status (the kubelet).
    fn set_pod(&self, key: &(String, String), f: impl FnOnce(&mut Value)) {
        let mut o = self.objs.lock().unwrap();
        let Some(mut p) = o.pods.remove(key) else {
            return;
        };
        f(&mut p.obj);
        Self::bump(&mut o, &mut p.obj);
        o.pods.insert(key.clone(), p);
    }

    /// The garbage collector: deletes every object owned by one of `uids`, then their
    /// dependents.
    fn collect(&self, mut uids: Vec<String>) {
        let owned = |obj: &Value, uids: &[String]| {
            obj.pointer("/metadata/ownerReferences")
                .and_then(Value::as_array)
                .is_some_and(|refs| {
                    refs.iter().any(|r| {
                        r["uid"]
                            .as_str()
                            .is_some_and(|u| uids.iter().any(|x| x == u))
                    })
                })
        };
        let uid = |obj: &Value| {
            obj.pointer("/metadata/uid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        while !uids.is_empty() {
            let mut next = Vec::new();
            let pods: Vec<(String, String)> = {
                let mut o = self.objs.lock().unwrap();
                let cms: Vec<_> = o
                    .cms
                    .iter()
                    .filter(|(_, v)| owned(v, &uids))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in cms {
                    if let Some(v) = o.cms.remove(&k) {
                        next.push(uid(&v));
                    }
                }
                let secrets: Vec<_> = o
                    .secrets
                    .iter()
                    .filter(|(_, v)| owned(v, &uids))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in secrets {
                    if let Some(v) = o.secrets.remove(&k) {
                        next.push(uid(&v));
                    }
                }
                o.pods
                    .iter()
                    .filter(|(_, p)| owned(&p.obj, &uids))
                    .map(|(k, p)| {
                        next.push(uid(&p.obj));
                        k.clone()
                    })
                    .collect()
            };
            for (ns, name) in pods {
                self.remove_pod(&ns, &name);
            }
            uids = next;
        }
    }

    /// Runs the garbage collector for a deleted object (at once or after `gc_delay_ms`).
    fn deleted(self: &Arc<Self>, uid: String) {
        let delay = self.gc_delay_ms.load(Ordering::SeqCst);
        if delay == 0 {
            self.collect(vec![uid]);
        } else {
            let f = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay)).await;
                f.collect(vec![uid]);
            });
        }
    }

    /// The garbage collector for a new object whose owner does not exist: it is deleted (at once
    /// or after `gc_delay_ms`), as a real collector deletes a dependent of an absent owner.
    fn orphans(self: &Arc<Self>, obj: &Value) {
        let refs: Vec<String> = obj
            .pointer("/metadata/ownerReferences")
            .and_then(Value::as_array)
            .map(|r| {
                r.iter()
                    .filter_map(|x| x["uid"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if refs.is_empty() {
            return;
        }
        let exists = |u: &str| {
            let o = self.objs.lock().unwrap();
            let has = |v: &Value| v.pointer("/metadata/uid").and_then(Value::as_str) == Some(u);
            o.cms.values().any(has)
                || o.secrets.values().any(has)
                || o.pods.values().any(|p| has(&p.obj))
        };
        if let Some(gone) = refs.into_iter().find(|u| !exists(u)) {
            self.deleted(gone);
        }
    }

    /// The Secrets of the namespace (names and data), as the tests see them.
    pub fn secrets(&self) -> Vec<Value> {
        self.objs
            .lock()
            .unwrap()
            .secrets
            .values()
            .cloned()
            .collect()
    }

    fn secret_value(&self, ns: &str, secret: &str, key: &str) -> Option<String> {
        let o = self.objs.lock().unwrap();
        o.secrets
            .get(&(ns.to_owned(), secret.to_owned()))
            .and_then(|s| s.pointer(&format!("/data/{key}")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// The pod rules of `deploy/staging/k8s/manifests/apps-admission.yaml` (policy
/// `ruum-apps-gvisor`, backlog S-83 and S-89), in the same order and with the same meaning.
pub fn admit(pod: &Value) -> Result<(), &'static str> {
    let spec = &pod["spec"];
    let arr = |v: &Value| v.as_array().cloned().unwrap_or_default();
    if spec["runtimeClassName"] != json!("gvisor") {
        return Err("pods in ruum-apps run in gVisor only");
    }
    if spec["automountServiceAccountToken"] != json!(false) {
        return Err("no automounted service account token");
    }
    if spec["hostNetwork"] == json!(true) {
        return Err("never the node's network");
    }
    if !arr(&spec["ephemeralContainers"]).is_empty() {
        return Err("no ephemeral containers");
    }
    let inits = arr(&spec["initContainers"]);
    let init_ok = inits.len() == 1
        && inits[0]["name"] == json!("ruum-supervisor")
        && arr(&inits[0]["volumeMounts"]).len() == 1
        && inits[0]["volumeMounts"][0]["name"] == json!("ruum-bin");
    if !init_ok {
        return Err("exactly the init container ruum-supervisor, which mounts only ruum-bin");
    }
    if spec["serviceAccountName"] != json!("ruum-sandbox") {
        return Err("the service account ruum-sandbox only");
    }
    for v in arr(&spec["volumes"]) {
        let empty_dir = v.get("emptyDir").is_some();
        let sources = arr(&v["projected"]["sources"]);
        let token = v.get("projected").is_some()
            && sources.len() == 1
            && sources[0]["serviceAccountToken"]["audience"] == json!("ruum-supervisor")
            && sources[0]["serviceAccountToken"]["expirationSeconds"] == json!(600)
            && sources[0].as_object().is_some_and(|o| o.len() == 1);
        let only = v.as_object().is_some_and(|o| o.len() == 2);
        if !(only && (empty_dir || token)) {
            return Err("volumes: only emptyDir and the supervisor's projected token");
        }
    }
    if arr(&spec["volumes"])
        .iter()
        .filter(|v| v.get("projected").is_some())
        .count()
        != 1
    {
        return Err("exactly one projected volume");
    }
    for c in arr(&spec["containers"]).iter().chain(inits.iter()) {
        if !arr(&c["envFrom"]).is_empty() {
            return Err("no envFrom");
        }
        if arr(&c["env"]).iter().any(|e| e.get("valueFrom").is_some()) {
            return Err("no environment value from another object");
        }
        if c.get("terminationMessagePath")
            .is_some_and(|p| p != &json!("/dev/termination-log"))
            || c.get("terminationMessagePolicy")
                .is_some_and(|p| p != &json!("File"))
        {
            return Err("the default termination message path and policy File");
        }
    }
    for c in arr(&spec["containers"]) {
        let mounts = arr(&c["volumeMounts"]);
        if !mounts.iter().any(|m| m["name"] == json!("ruum-bin"))
            || mounts
                .iter()
                .any(|m| m["name"] == json!("ruum-bin") && m["readOnly"] != json!(true))
        {
            return Err("ruum-bin is read-only in every container");
        }
    }
    Ok(())
}

/// The Secret rule of `deploy/staging/k8s/manifests/apps-admission.yaml` (policy
/// `ruum-apps-secrets`, backlog S-89): no Secret in ruum-apps at all.
pub fn admit_secret(_secret: &Value) -> Result<(), &'static str> {
    Err("no Secret in ruum-apps (the sandbox values are sealed in the records, backlog S-89)")
}

fn plural_ok(kind: &str) -> bool {
    matches!(kind, "pods" | "configmaps" | "secrets")
}

async fn collection_get(
    State(f): State<Arc<Fake>>,
    Path((ns, kind)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !plural_ok(&kind) {
        return status(StatusCode::NOT_FOUND, "no such kind");
    }
    if let Err(r) = f.allow(&headers, &ns, "list", &kind) {
        return r;
    }
    let o = f.objs.lock().unwrap();
    let sel = q.get("labelSelector");
    let items: Vec<Value> = match kind.as_str() {
        "pods" => o
            .pods
            .iter()
            .filter(|((n, _), _)| *n == ns)
            .map(|(_, p)| p.obj.clone())
            .collect(),
        "configmaps" => o
            .cms
            .iter()
            .filter(|((n, _), _)| *n == ns)
            .map(|(_, v)| v.clone())
            .collect(),
        _ => o
            .secrets
            .iter()
            .filter(|((n, _), _)| *n == ns)
            .map(|(_, v)| v.clone())
            .collect(),
    };
    let hide = kind == "configmaps" && f.hide_records_once.swap(false, Ordering::SeqCst);
    let items: Vec<Value> = items
        .into_iter()
        .filter(|i| !hide && matches_selector(i, sel))
        .collect();
    axum::Json(json!({ "kind": "List", "items": items })).into_response()
}

async fn collection_post(
    State(f): State<Arc<Fake>>,
    Path((ns, kind)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !plural_ok(&kind) {
        return status(StatusCode::NOT_FOUND, "no such kind");
    }
    if let Err(r) = f.allow(&headers, &ns, "create", &kind) {
        return r;
    }
    let Ok(mut obj) = serde_json::from_slice::<Value>(&body) else {
        return status(StatusCode::BAD_REQUEST, "not JSON");
    };
    let Some(name) = obj
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return status(StatusCode::UNPROCESSABLE_ENTITY, "no name");
    };
    obj["metadata"]["namespace"] = json!(ns);
    obj["metadata"]["uid"] = json!(uuid::Uuid::now_v7().to_string());
    let key = (ns.clone(), name.clone());
    match kind.as_str() {
        "configmaps" => {
            {
                let mut o = f.objs.lock().unwrap();
                if o.cms.contains_key(&key) {
                    return status(StatusCode::CONFLICT, "AlreadyExists");
                }
                Fake::bump(&mut o, &mut obj);
                o.cms.insert(key, obj.clone());
            }
            f.orphans(&obj);
            (StatusCode::CREATED, axum::Json(obj)).into_response()
        }
        "secrets" => {
            if let Err(why) = admit_secret(&obj) {
                return status(StatusCode::FORBIDDEN, &format!("admission: {why}"));
            }
            let mut o = f.objs.lock().unwrap();
            if o.secrets.contains_key(&key) {
                return status(StatusCode::CONFLICT, "AlreadyExists");
            }
            if let Some(sd) = obj.get("stringData").cloned() {
                obj["data"] = sd;
                obj.as_object_mut().unwrap().remove("stringData");
            }
            Fake::bump(&mut o, &mut obj);
            o.secrets.insert(key, obj.clone());
            drop(o);
            f.orphans(&obj);
            (StatusCode::CREATED, axum::Json(obj)).into_response()
        }
        _ => {
            // Admission: the namespace's rules (backlog S-83).
            if let Err(why) = admit(&obj) {
                return status(StatusCode::FORBIDDEN, &format!("admission: {why}"));
            }
            // The API server's canonical quantities.
            for kind in ["limits", "requests"] {
                if let Some(m) = obj
                    .pointer(&format!("/spec/containers/0/resources/{kind}/memory"))
                    .and_then(Value::as_str)
                    .map(canonical_bytes)
                {
                    obj["spec"]["containers"][0]["resources"][kind]["memory"] = json!(m);
                }
            }
            if let Some(vols) = obj
                .pointer_mut("/spec/volumes")
                .and_then(Value::as_array_mut)
            {
                for v in vols {
                    if let Some(s) = v
                        .pointer("/emptyDir/sizeLimit")
                        .and_then(Value::as_str)
                        .map(canonical_bytes)
                    {
                        v["emptyDir"]["sizeLimit"] = json!(s);
                    }
                }
            }
            match *f.tamper.lock().unwrap() {
                Tamper::None => {}
                Tamper::AddCapability => {
                    obj["spec"]["containers"][0]["securityContext"]["capabilities"]["add"] =
                        json!(["NET_RAW"]);
                }
                Tamper::DropRuntimeClass => {
                    obj["spec"]["runtimeClassName"] = json!("runc");
                }
                Tamper::InjectSecretEnv => {
                    if let Some(env) = obj
                        .pointer_mut("/spec/containers/0/env")
                        .and_then(Value::as_array_mut)
                    {
                        env.push(json!({ "name": "STOLEN", "valueFrom": { "secretKeyRef": {
                            "name": "ruum-i-00000000000000000000000000000000", "key": "token" } } }));
                    }
                }
                Tamper::ChangeHostAlias => {
                    obj["spec"]["hostAliases"] =
                        json!([{ "ip": "203.0.113.9", "hostnames": ["ruum-egress"] }]);
                }
                Tamper::ServiceAccount => {
                    obj["spec"]["serviceAccountName"] = json!("ruum-server");
                }
                Tamper::HostPort => {
                    obj["spec"]["containers"][0]["ports"][0]["hostPort"] = json!(8080);
                }
                Tamper::WritableSupervisor => {
                    if let Some(m) = obj
                        .pointer_mut("/spec/containers/0/volumeMounts")
                        .and_then(Value::as_array_mut)
                    {
                        for x in m.iter_mut() {
                            if x["name"] == json!("ruum-bin") {
                                x["readOnly"] = json!(false);
                            }
                        }
                    }
                }
                Tamper::TokenAudience => {
                    if let Some(v) = obj
                        .pointer_mut("/spec/volumes")
                        .and_then(Value::as_array_mut)
                    {
                        for x in v.iter_mut() {
                            if let Some(t) =
                                x.pointer_mut("/projected/sources/0/serviceAccountToken")
                            {
                                t["audience"] = json!("other-audience");
                            }
                        }
                    }
                }
            }
            obj["status"] = json!({ "phase": "Pending" });
            let log = Arc::new(LogBuf::default());
            {
                let mut o = f.objs.lock().unwrap();
                if o.pods.contains_key(&key) {
                    return status(StatusCode::CONFLICT, "AlreadyExists");
                }
                Fake::bump(&mut o, &mut obj);
                o.pods.insert(
                    key.clone(),
                    PodState {
                        obj: obj.clone(),
                        log: log.clone(),
                        task: None,
                        started: false,
                    },
                );
            }
            let task = tokio::spawn(kubelet(f.clone(), key.clone(), log));
            if let Some(p) = f.objs.lock().unwrap().pods.get_mut(&key) {
                p.task = Some(task);
            }
            (StatusCode::CREATED, axum::Json(obj)).into_response()
        }
    }
}

async fn object_get(
    State(f): State<Arc<Fake>>,
    Path((ns, kind, name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if !plural_ok(&kind) {
        return status(StatusCode::NOT_FOUND, "no such kind");
    }
    if let Err(r) = f.allow(&headers, &ns, "get", &kind) {
        return r;
    }
    let key = (ns, name);
    let o = f.objs.lock().unwrap();
    let v = match kind.as_str() {
        "pods" => o.pods.get(&key).map(|p| p.obj.clone()),
        "configmaps" => o.cms.get(&key).cloned(),
        _ => o.secrets.get(&key).cloned(),
    };
    match v {
        Some(v) => axum::Json(v).into_response(),
        None => status(StatusCode::NOT_FOUND, "NotFound"),
    }
}

async fn object_put(
    State(f): State<Arc<Fake>>,
    Path((ns, kind, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = f.allow(&headers, &ns, "update", &kind) {
        return r;
    }
    if kind != "configmaps" {
        return status(
            StatusCode::METHOD_NOT_ALLOWED,
            "the fake updates ConfigMaps only",
        );
    }
    let Ok(mut obj) = serde_json::from_slice::<Value>(&body) else {
        return status(StatusCode::BAD_REQUEST, "not JSON");
    };
    let key = (ns.clone(), name);
    let mut o = f.objs.lock().unwrap();
    let Some(cur) = o.cms.get(&key) else {
        return status(StatusCode::NOT_FOUND, "NotFound");
    };
    if obj.pointer("/metadata/resourceVersion") != cur.pointer("/metadata/resourceVersion") {
        return status(StatusCode::CONFLICT, "the object has been modified");
    }
    obj["metadata"]["namespace"] = json!(ns);
    // The uid and the owners are the stored object's (a real API server keeps them).
    obj["metadata"]["uid"] = cur["metadata"]["uid"].clone();
    if obj.pointer("/metadata/ownerReferences").is_none()
        && let Some(r) = cur.pointer("/metadata/ownerReferences").cloned()
    {
        obj["metadata"]["ownerReferences"] = r;
    }
    Fake::bump(&mut o, &mut obj);
    o.cms.insert(key, obj.clone());
    axum::Json(obj).into_response()
}

async fn object_patch(
    State(f): State<Arc<Fake>>,
    Path((ns, kind, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = f.allow(&headers, &ns, "patch", &kind) {
        return r;
    }
    if headers.get("content-type").and_then(|v| v.to_str().ok())
        != Some("application/merge-patch+json")
    {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE, "merge patches only");
    }
    let Ok(mut patch) = serde_json::from_slice::<Value>(&body) else {
        return status(StatusCode::BAD_REQUEST, "not JSON");
    };
    let key = (ns, name);
    let mut o = f.objs.lock().unwrap();
    let mut cur = match kind.as_str() {
        "secrets" => match o.secrets.get(&key) {
            Some(s) => s.clone(),
            None => return status(StatusCode::NOT_FOUND, "NotFound"),
        },
        _ => {
            return status(
                StatusCode::METHOD_NOT_ALLOWED,
                "the fake patches Secrets only",
            );
        }
    };
    if let Some(sd) = patch.get("stringData").cloned() {
        patch["data"] = sd;
        patch.as_object_mut().unwrap().remove("stringData");
    }
    merge(&mut cur, &patch);
    Fake::bump(&mut o, &mut cur);
    o.secrets.insert(key, cur.clone());
    axum::Json(cur).into_response()
}

async fn object_delete(
    State(f): State<Arc<Fake>>,
    Path((ns, kind, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(r) = f.allow(&headers, &ns, "delete", &kind) {
        return r;
    }
    let policy = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|b| b["propagationPolicy"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let key = (ns.clone(), name.clone());
    let uid = |v: &Value| {
        v.pointer("/metadata/uid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    // A real API server adds the finalizer of a foreground or orphan deletion and answers with
    // the object as it is now: for a Secret, its data.
    if kind == "secrets" && matches!(policy.as_str(), "Foreground" | "Orphan") {
        let s = f.objs.lock().unwrap().secrets.remove(&key);
        return match s {
            Some(s) => {
                f.deleted(uid(&s));
                axum::Json(s).into_response()
            }
            None => status(StatusCode::NOT_FOUND, "NotFound"),
        };
    }
    let found = match kind.as_str() {
        "pods" => {
            let had = f.pod(&ns, &name).map(|p| uid(&p));
            f.remove_pod(&ns, &name);
            had
        }
        "configmaps" => f.objs.lock().unwrap().cms.remove(&key).map(|v| uid(&v)),
        "secrets" => f.objs.lock().unwrap().secrets.remove(&key).map(|v| uid(&v)),
        _ => None,
    };
    match found {
        Some(u) => {
            f.deleted(u);
            axum::Json(json!({ "kind": "Status", "status": "Success" })).into_response()
        }
        None => status(StatusCode::NOT_FOUND, "NotFound"),
    }
}

async fn pod_log(
    State(f): State<Arc<Fake>>,
    Path((ns, name)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = f.allow(&headers, &ns, "get", "pods/log") {
        return r;
    }
    if q.get("follow").map(String::as_str) != Some("true")
        || q.get("container").map(String::as_str) != Some("app")
    {
        return status(
            StatusCode::BAD_REQUEST,
            "the fake follows the container app only",
        );
    }
    let (log, started) = {
        let o = f.objs.lock().unwrap();
        match o.pods.get(&(ns, name)) {
            Some(p) => (p.log.clone(), p.started),
            None => return status(StatusCode::NOT_FOUND, "NotFound"),
        }
    };
    if f.log_not_ready_once.swap(false, Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(300)).await;
        return status(
            StatusCode::BAD_REQUEST,
            "container \"app\" in pod is waiting to start: ContainerCreating",
        );
    }
    if !started {
        return status(
            StatusCode::BAD_REQUEST,
            "container \"app\" in pod is waiting to start: ContainerCreating",
        );
    }
    let want: Option<u8> = match q.get("stream").map(String::as_str) {
        None | Some("All") => None,
        Some("Stdout") => Some(1),
        Some("Stderr") => Some(2),
        Some(_) => return status(StatusCode::BAD_REQUEST, "bad stream"),
    };
    let from = if q.get("tailLines").map(String::as_str) == Some("0") {
        log.chunks.lock().unwrap().len()
    } else {
        0
    };
    let stream = futures::stream::unfold((log, from), move |(log, mut at)| async move {
        loop {
            let notified = log.notify.notified();
            let next = {
                let chunks = log.chunks.lock().unwrap();
                let mut found = None;
                while at < chunks.len() {
                    let (s, b) = &chunks[at];
                    at += 1;
                    if want.is_none_or(|w| w == *s) {
                        found = Some(b.clone());
                        break;
                    }
                }
                found
            };
            if let Some(b) = next {
                drop(notified);
                return Some((Ok::<Bytes, std::io::Error>(b), (log, at)));
            }
            if log.ended.load(Ordering::SeqCst) && at >= log.chunks.lock().unwrap().len() {
                return None;
            }
            notified.await;
        }
    });
    Response::new(Body::from_stream(stream))
}

/// `POST /apis/authentication.k8s.io/v1/tokenreviews` (cluster-scoped, the ClusterRole).
async fn token_review(State(f): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = f.allow(&headers, "", "create", "tokenreviews") {
        return r;
    }
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return status(StatusCode::BAD_REQUEST, "not JSON");
    };
    let token = req
        .pointer("/spec/token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let wanted: Vec<String> = req
        .pointer("/spec/audiences")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let issued = f.issued.lock().unwrap().get(token).cloned();
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let st = match issued {
        Some(i) if i.exp > now && wanted.contains(&i.audience) => json!({
            "authenticated": true,
            "audiences": [i.audience],
            "user": {
                "username": format!("system:serviceaccount:{}:{}", i.namespace, i.service_account),
                "groups": ["system:serviceaccounts"],
                "extra": {
                    "authentication.kubernetes.io/pod-name": [i.pod],
                    "authentication.kubernetes.io/pod-uid": [i.pod_uid],
                },
            },
        }),
        Some(_) => json!({ "authenticated": false, "error": "audience or expiry" }),
        None => json!({ "authenticated": false, "error": "invalid bearer token" }),
    };
    (
        StatusCode::CREATED,
        axum::Json(json!({ "apiVersion": "authentication.k8s.io/v1", "kind": "TokenReview", "status": st })),
    )
        .into_response()
}

async fn registry(
    State(f): State<Arc<Fake>>,
    method: Method,
    Path(rest): Path<String>,
) -> Response {
    let (repo, digest) = match rest.split_once("/manifests/") {
        Some(x) => x,
        None => match rest.split_once("/blobs/") {
            Some(x) => x,
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    if !f
        .registry
        .lock()
        .unwrap()
        .contains(&(repo.to_owned(), digest.to_owned()))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if method == Method::HEAD {
        return StatusCode::OK.into_response();
    }
    match f.blobs.lock().unwrap().get(digest) {
        Some(b) => (StatusCode::OK, b.clone()).into_response(),
        None => StatusCode::OK.into_response(),
    }
}

/// The fake sandbox supervisor of one pod: the launch values, the current token, one redactor
/// per stream (`ruum-core`'s, as the real supervisor).
struct Sup {
    out: LineRedactor,
    err: LineRedactor,
    base: Vec<String>,
    token: String,
    url: String,
    bearer: String,
    peer: String,
}

impl Sup {
    fn values(&self) -> Vec<String> {
        let mut v = self.base.clone();
        v.push(self.token.clone());
        v
    }

    /// Redacts a chunk of one stream; the complete lines, each with its line end.
    fn push(&mut self, stream: u8, data: &[u8]) -> Vec<u8> {
        let now = time::OffsetDateTime::now_utc();
        let lines = if stream == 1 {
            self.out.push_bytes(data, now)
        } else {
            self.err.push_bytes(data, now)
        };
        let mut out = Vec::new();
        for (_, l) in lines {
            out.extend_from_slice(l.as_bytes());
            out.push(b'\n');
        }
        out
    }

    fn finish(&mut self) -> (Vec<u8>, Vec<u8>) {
        let now = time::OffsetDateTime::now_utc();
        let join = |lines: Vec<(time::OffsetDateTime, String)>| {
            let mut out = Vec::new();
            for (_, l) in lines {
                out.extend_from_slice(l.as_bytes());
                out.push(b'\n');
            }
            out
        };
        (join(self.out.finish(now)), join(self.err.finish(now)))
    }

    /// The current token from `/v1/token`, redacted before it is used (as the supervisor).
    async fn refresh(&mut self) {
        let r = reqwest::Client::new()
            .post(format!("{}/v1/token", self.url))
            .bearer_auth(&self.bearer)
            .header("x-fake-peer", &self.peer)
            .send()
            .await;
        if let Ok(r) = r
            && r.status() == StatusCode::OK
            && let Ok(a) = r.json::<TokenAnswer>().await
            && a.check().is_ok()
            && a.token != self.token
        {
            self.base.push(std::mem::replace(&mut self.token, a.token));
            let all = self.values();
            self.out.set_values(all.iter().map(String::as_str));
            self.err.set_values(all.iter().map(String::as_str));
        }
    }
}

/// The value after `flag` in a command.
fn arg_after(cmd: &[String], flag: &str) -> Option<String> {
    cmd.iter()
        .position(|a| a == flag)
        .and_then(|i| cmd.get(i + 1))
        .cloned()
}

/// Runs a pod (see the module docs): the kubelet, the supervisor's launch and the program.
async fn kubelet(f: Arc<Fake>, key: (String, String), log: Arc<LogBuf>) {
    let Some(pod) = f.pod(&key.0, &key.1) else {
        return;
    };
    let c = pod
        .pointer("/spec/containers/0")
        .cloned()
        .unwrap_or_default();
    let ns = key.0.clone();
    // Plain environment values (admission refuses any other kind).
    let mut env: HashMap<String, String> = HashMap::new();
    for e in c
        .get("env")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if let (Some(n), Some(v)) = (e["name"].as_str(), e["value"].as_str()) {
            env.insert(n.to_owned(), v.to_owned());
        }
    }
    let memory_limit = c
        .pointer("/resources/limits/memory")
        .and_then(Value::as_str)
        .map(bytes_of)
        .unwrap_or(u64::MAX);
    let disk_limit = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .and_then(|v| {
            v.iter()
                .find(|v| v["name"] == "tmp")
                .and_then(|v| v.pointer("/emptyDir/sizeLimit"))
                .and_then(Value::as_str)
                .map(bytes_of)
        })
        .unwrap_or(u64::MAX);
    let has_probe = c.get("readinessProbe").is_some();
    let ip = format!(
        "10.244.0.{}",
        f.next_ip.fetch_add(1, Ordering::SeqCst) % 250 + 2
    );
    let started_at = now_rfc3339();
    f.set_pod(&key, |p| {
        p["status"] = json!({
            "phase": "Running",
            "podIP": ip,
            "podIPs": [{ "ip": ip }],
            "containerStatuses": [{ "name": "app", "ready": false,
                "state": { "running": { "startedAt": started_at } } }],
        });
    });
    if let Some(p) = f.objs.lock().unwrap().pods.get_mut(&key) {
        p.started = true;
    }
    // The deadline of a job.
    if let Some(secs) = pod
        .pointer("/spec/activeDeadlineSeconds")
        .and_then(Value::as_u64)
    {
        let (f2, key2, log2) = (f.clone(), key.clone(), log.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let alive = f2.objs.lock().unwrap().pods.contains_key(&key2);
            let done = f2
                .pod(&key2.0, &key2.1)
                .and_then(|p| {
                    p.pointer("/status/phase")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .is_some_and(|ph| ph != "Running");
            if alive && !done {
                if let Some(t) = f2
                    .objs
                    .lock()
                    .unwrap()
                    .pods
                    .get_mut(&key2)
                    .and_then(|p| p.task.take())
                {
                    t.abort();
                }
                f2.set_pod(&key2, |p| {
                    p["status"]["phase"] = json!("Failed");
                    p["status"]["reason"] = json!("DeadlineExceeded");
                    p["status"]["containerStatuses"] = json!([{ "name": "app", "ready": false,
                        "state": { "terminated": { "exitCode": 137, "reason": "Error" } } }]);
                });
                log2.end();
            }
        });
    }
    let end = |code: i64, reason: &str| {
        let reason = reason.to_owned();
        f.set_pod(&key, move |p| {
            p["status"]["phase"] = json!(if code == 0 { "Succeeded" } else { "Failed" });
            p["status"]["containerStatuses"] = json!([{ "name": "app", "ready": false,
                "state": { "terminated": { "exitCode": code, "reason": reason } } }]);
        });
        log.end();
    };
    let ready = |r: bool| {
        f.set_pod(&key, move |p| {
            p["status"]["containerStatuses"][0]["ready"] = json!(r);
        });
    };
    // The projected token of the pod (the kubelet issues it for this pod only).
    let token = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .and_then(|v| {
            v.iter().find_map(|v| {
                v.pointer("/projected/sources/0/serviceAccountToken")
                    .cloned()
            })
        })
        .map(|t| {
            f.issue(Issued {
                audience: t["audience"].as_str().unwrap_or_default().to_owned(),
                namespace: ns.clone(),
                service_account: pod
                    .pointer("/spec/serviceAccountName")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned(),
                pod: key.1.clone(),
                pod_uid: pod
                    .pointer("/metadata/uid")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                exp: time::OffsetDateTime::now_utc().unix_timestamp()
                    + t["expirationSeconds"].as_i64().unwrap_or(3600),
            })
        });
    let command: Vec<String> = c
        .get("command")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    // The supervisor: one launch, then the program of its command with every line redacted.
    if command.first().map(String::as_str) == Some(SUPERVISOR_PATH)
        && f.skip_launch_once.swap(false, Ordering::SeqCst)
    {
        ready(false);
        std::future::pending::<()>().await;
    }
    let (mut sup, args) = if command.first().map(String::as_str) == Some(SUPERVISOR_PATH) {
        let (Some(url), Some(bearer)) = (arg_after(&command, "--launch-url"), token) else {
            log.push(2, b"ruum-supervisor: no launch channel\n".to_vec());
            end(125, "Error");
            return;
        };
        // As the real supervisor: 503, 429 and transport errors are tried again (the seal of
        // the pod's run is written right after the pod), anything else ends the launch.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let (code, body) = loop {
            let r = reqwest::Client::new()
                .post(format!("{url}/v1/launch"))
                .bearer_auth(&bearer)
                .header("x-fake-peer", &ip)
                .send()
                .await;
            let (code, body) = match r {
                Ok(r) => {
                    let code = r.status().as_u16();
                    (code, r.bytes().await.unwrap_or_default())
                }
                Err(_) => (0, Bytes::new()),
            };
            if matches!(code, 0 | 429 | 503) && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(30)).await;
                continue;
            }
            break (code, body);
        };
        f.launches.lock().unwrap().push((key.1.clone(), code));
        let launch = match ruum_supervisor::run::parse_launch(&body, true) {
            Ok(l) if code == 200 => l,
            _ => {
                log.push(
                    2,
                    format!("ruum-supervisor: the launch failed: status {code}\n").into_bytes(),
                );
                end(125, "Error");
                return;
            }
        };
        if launch.argv.first().map(String::as_str) != Some("/ruum-program") {
            log.push(
                2,
                b"ruum-supervisor: the app's command did not start\n".to_vec(),
            );
            end(127, "Error");
            return;
        }
        for (k, v) in &launch.env {
            env.insert(k.clone(), v.clone());
        }
        let values = launch.values();
        let red =
            || LineRedactor::new(values.iter().map(String::as_str)).with_max_line_bytes(16 * 1024);
        let token = launch.token.clone().unwrap_or_default();
        let mut base = values.clone();
        base.retain(|v| *v != token);
        let sup = Sup {
            out: red(),
            err: red(),
            base,
            token,
            url,
            bearer,
            peer: ip.clone(),
        };
        (Some(sup), launch.argv[1..].to_vec())
    } else {
        // Not the supervisor (a pod made by hand): its arguments, unredacted.
        let args: Vec<String> = c
            .get("args")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        (None, args)
    };
    let emit = |sup: &mut Option<Sup>, stream: u8, data: Vec<u8>| match sup {
        Some(s) => {
            let out = s.push(stream, &data);
            if !out.is_empty() {
                log.push(stream, out);
            }
        }
        None => log.push(stream, data),
    };
    let flush = |sup: &mut Option<Sup>| {
        if let Some(s) = sup {
            let (o, e) = s.finish();
            if !o.is_empty() {
                log.push(1, o);
            }
            if !e.is_empty() {
                log.push(2, e);
            }
        }
    };
    let (mut mem, mut disk) = (0u64, 0u64);
    for arg in args {
        let Some(step) = Step::parse(&arg) else {
            emit(&mut sup, 2, format!("unknown step {arg}\n").into_bytes());
            flush(&mut sup);
            end(2, "Error");
            return;
        };
        match step {
            Step::Print(t) => emit(&mut sup, 1, format!("{t}\n").into_bytes()),
            Step::Write(t) => emit(&mut sup, 1, t.into_bytes()),
            Step::Stderr(t) => emit(&mut sup, 2, format!("{t}\n").into_bytes()),
            Step::StderrWrite(t) => emit(&mut sup, 2, t.into_bytes()),
            Step::Env(n) => {
                let v = env.get(&n).cloned().unwrap_or_default();
                emit(&mut sup, 1, format!("{n}={v}\n").into_bytes());
            }
            Step::Token | Step::TokenSha => {
                // The supervisor writes the token file; the program reads the newest one.
                let t = match &mut sup {
                    Some(s) => {
                        s.refresh().await;
                        s.token.clone()
                    }
                    None => String::new(),
                };
                let line = if step == Step::Token {
                    t.clone()
                } else {
                    token_sha(&t)
                };
                emit(&mut sup, 1, format!("{line}\n").into_bytes());
            }
            Step::Sleep(d) => tokio::time::sleep(d).await,
            Step::Exit(code) => {
                flush(&mut sup);
                end(
                    i64::from(code),
                    if code == 0 { "Completed" } else { "Error" },
                );
                return;
            }
            Step::Serve => {
                flush(&mut sup);
                ready(has_probe);
                std::future::pending::<()>().await;
            }
            Step::Unhealthy => {
                flush(&mut sup);
                ready(false);
                std::future::pending::<()>().await;
            }
            Step::Alloc(b) => {
                mem += b;
                if mem > memory_limit {
                    flush(&mut sup);
                    end(137, "OOMKilled");
                    return;
                }
            }
            Step::Disk(b) => {
                if disk + b > disk_limit {
                    emit(&mut sup, 1, format!("{DISK_FULL}\n").into_bytes());
                    flush(&mut sup);
                    end(1, "Error");
                    return;
                }
                disk += b;
            }
        }
    }
    flush(&mut sup);
    end(0, "Completed");
}
