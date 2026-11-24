//! A fake Kubernetes API server with a fake kubelet, for the Kubernetes driver's tests (I6-1).
//!
//! - **The API**: pods, ConfigMaps and Secrets of namespaces, pod logs (followed, one stream or
//!   both), label selectors, resourceVersion conflicts on `PUT`, merge patches with
//!   `stringData`. A bearer token is required.
//! - **RBAC**: the rules of the driver's Role are read from the chart
//!   (`deploy/staging/charts/platform/templates/runtime-rbac.yaml`) and every other verb,
//!   resource or namespace is `403`. So a driver change that needs a new right fails here until
//!   the Role has it.
//! - **Admission**: the namespace's rule of backlog S-83 (`runtimeClassName: gvisor`) and no
//!   service account token; quantities are written back in the API server's canonical form
//!   (`64Mi`), as a real API server does.
//! - **The kubelet**: each pod runs the conformance program of its arguments
//!   (`ruum_core::runtime::program`): output into the log, environment values from the pod
//!   (Secret keys resolved at start), the token read from the Secret at each read (a real
//!   kubelet refreshes the file within a minute), the memory limit (`OOMKilled`), the size of the
//!   memory-backed `/tmp` (a full disk), `activeDeadlineSeconds` (`DeadlineExceeded`) and the
//!   readiness of `serve` and `unhealthy`.
//! - **The registry**: `HEAD /v2/<repo>/manifests/<digest>` answers for pushed images only.

#![allow(dead_code)]

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ruum_core::runtime::program::{DISK_FULL, Step, token_sha};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
}

pub struct Fake {
    objs: Mutex<Objects>,
    rules: Vec<(String, Vec<String>)>,
    namespace: String,
    pub calls: Mutex<Vec<(String, String, String)>>,
    registry: Mutex<HashSet<(String, String)>>,
    pub tamper: Mutex<Tamper>,
    next_ip: AtomicU32,
}

/// The Role of the driver, read from the chart: `(resource, verbs)`.
pub fn role_rules() -> Vec<(String, Vec<String>)> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/staging/charts/platform/templates/runtime-rbac.yaml"
    );
    let text = std::fs::read_to_string(path).expect("the driver's Role in the chart");
    let mut out = Vec::new();
    let mut in_role = false;
    for line in text.lines() {
        if line.starts_with("kind: ") {
            in_role = line == "kind: Role";
        }
        let l = line.trim();
        if !in_role || !l.starts_with("- { apiGroups: [\"\"], resources: [") {
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
    assert!(!out.is_empty(), "no rules read from {path}");
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
            namespace: namespace.to_owned(),
            calls: Mutex::new(Vec::new()),
            registry: Mutex::new(HashSet::new()),
            tamper: Mutex::new(Tamper::None),
            next_ip: AtomicU32::new(2),
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
        let ok = ns == self.namespace
            && self
                .rules
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

    fn secret_value(&self, ns: &str, secret: &str, key: &str) -> Option<String> {
        let o = self.objs.lock().unwrap();
        o.secrets
            .get(&(ns.to_owned(), secret.to_owned()))
            .and_then(|s| s.pointer(&format!("/data/{key}")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
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
    let items: Vec<Value> = items
        .into_iter()
        .filter(|i| matches_selector(i, sel))
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
    let key = (ns.clone(), name.clone());
    match kind.as_str() {
        "configmaps" => {
            let mut o = f.objs.lock().unwrap();
            if o.cms.contains_key(&key) {
                return status(StatusCode::CONFLICT, "AlreadyExists");
            }
            Fake::bump(&mut o, &mut obj);
            o.cms.insert(key, obj.clone());
            (StatusCode::CREATED, axum::Json(obj)).into_response()
        }
        "secrets" => {
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
            (StatusCode::CREATED, axum::Json(obj)).into_response()
        }
        _ => {
            // Admission: the namespace's rule (backlog S-83) and no token.
            if obj.pointer("/spec/runtimeClassName") != Some(&json!("gvisor")) {
                return status(
                    StatusCode::FORBIDDEN,
                    "admission: pods in ruum-apps run in gVisor only",
                );
            }
            if obj.pointer("/spec/automountServiceAccountToken") != Some(&json!(false)) {
                return status(StatusCode::FORBIDDEN, "admission: no service account token");
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
) -> Response {
    if let Err(r) = f.allow(&headers, &ns, "delete", &kind) {
        return r;
    }
    let key = (ns.clone(), name.clone());
    let found = match kind.as_str() {
        "pods" => {
            let had = f.objs.lock().unwrap().pods.contains_key(&key);
            f.remove_pod(&ns, &name);
            had
        }
        "configmaps" => f.objs.lock().unwrap().cms.remove(&key).is_some(),
        "secrets" => f.objs.lock().unwrap().secrets.remove(&key).is_some(),
        _ => false,
    };
    if found {
        axum::Json(json!({ "kind": "Status", "status": "Success" })).into_response()
    } else {
        status(StatusCode::NOT_FOUND, "NotFound")
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

async fn registry(
    State(f): State<Arc<Fake>>,
    method: Method,
    Path(rest): Path<String>,
) -> Response {
    let _ = method;
    let Some((repo, digest)) = rest.split_once("/manifests/") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if f.registry
        .lock()
        .unwrap()
        .contains(&(repo.to_owned(), digest.to_owned()))
    {
        StatusCode::OK.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Runs a pod's program (see the module docs).
async fn kubelet(f: Arc<Fake>, key: (String, String), log: Arc<LogBuf>) {
    let Some(pod) = f.pod(&key.0, &key.1) else {
        return;
    };
    let c = pod
        .pointer("/spec/containers/0")
        .cloned()
        .unwrap_or_default();
    let ns = key.0.clone();
    // Environment values; a missing Secret key never starts the container.
    let mut env: HashMap<String, String> = HashMap::new();
    for e in c
        .get("env")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let name = e["name"].as_str().unwrap().to_owned();
        let value = match e.get("value").and_then(Value::as_str) {
            Some(v) => Some(v.to_owned()),
            None => {
                let secret = e
                    .pointer("/valueFrom/secretKeyRef/name")
                    .and_then(Value::as_str);
                let k = e
                    .pointer("/valueFrom/secretKeyRef/key")
                    .and_then(Value::as_str);
                match (secret, k) {
                    (Some(s), Some(k)) => f.secret_value(&ns, s, k),
                    _ => None,
                }
            }
        };
        match value {
            Some(v) => {
                env.insert(name, v);
            }
            None => {
                f.set_pod(&key, |p| {
                    p["status"]["containerStatuses"] = json!([{ "name": "app", "ready": false,
                        "state": { "waiting": { "reason": "CreateContainerConfigError" } } }]);
                });
                return;
            }
        }
    }
    let token_secret = pod
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .and_then(|v| {
            v.iter()
                .find(|v| v["name"] == "token")
                .and_then(|v| v.pointer("/secret/secretName"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
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
    let args: Vec<String> = c
        .get("args")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let (mut mem, mut disk) = (0u64, 0u64);
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
    for arg in args {
        let Some(step) = Step::parse(&arg) else {
            log.push(2, format!("unknown step {arg}\n").into_bytes());
            end(2, "Error");
            return;
        };
        match step {
            Step::Print(t) => log.push(1, format!("{t}\n").into_bytes()),
            Step::Write(t) => log.push(1, t.into_bytes()),
            Step::Stderr(t) => log.push(2, format!("{t}\n").into_bytes()),
            Step::StderrWrite(t) => log.push(2, t.into_bytes()),
            Step::Env(n) => {
                let v = env.get(&n).cloned().unwrap_or_default();
                log.push(1, format!("{n}={v}\n").into_bytes());
            }
            Step::Token | Step::TokenSha => {
                let t = token_secret
                    .as_deref()
                    .and_then(|s| f.secret_value(&ns, s, "token"))
                    .unwrap_or_default();
                let line = if step == Step::Token {
                    t
                } else {
                    token_sha(&t)
                };
                log.push(1, format!("{line}\n").into_bytes());
            }
            Step::Sleep(d) => tokio::time::sleep(d).await,
            Step::Exit(code) => {
                end(
                    i64::from(code),
                    if code == 0 { "Completed" } else { "Error" },
                );
                return;
            }
            Step::Serve => {
                ready(has_probe);
                std::future::pending::<()>().await;
            }
            Step::Unhealthy => {
                ready(false);
                std::future::pending::<()>().await;
            }
            Step::Alloc(b) => {
                mem += b;
                if mem > memory_limit {
                    end(137, "OOMKilled");
                    return;
                }
            }
            Step::Disk(b) => {
                if disk + b > disk_limit {
                    log.push(1, format!("{DISK_FULL}\n").into_bytes());
                    end(1, "Error");
                    return;
                }
                disk += b;
            }
        }
    }
    end(0, "Completed");
}
