//! The stages of a deployment (`AppDeployment.stages`, an open set) and the stages that run
//! after the `202` of `createAppDeployment`: `image`, `start` and `check` for a static app.

use super::super::AppService;
use super::super::static_site::SiteIndex;
use crate::actor::Context;
use crate::error::ErrorCode;
use crate::ids::{NodeId, TenantId};
use crate::runtime::{
    self, ImageRef, InstanceState, Limits, RuntimeDriver, SecretValue, ServiceSpec, StaticImage,
    WorkloadIdentity, WorkloadKind, WorkloadSpec,
};
use crate::usage::{Charged, Meter, UsageEvent};
use serde_json::{Value, json};
use std::sync::Arc;
use time::OffsetDateTime;
use uuid::Uuid;

/// The stages in their order (`AppDeploymentStage.name`). `migrations` (D-2) and `switch` (a
/// release) are added when they run.
pub const STAGE_NAMES: &[&str] = &[
    "unpack", "validate", "build", "test", "bindings", "image", "start", "check",
];

fn time(t: OffsetDateTime) -> Value {
    Value::String(
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    )
}

fn stage(name: &str, state: &str, at: Option<OffsetDateTime>) -> Value {
    let done = matches!(state, "done" | "skipped" | "failed");
    json!({
        "name": name,
        "state": state,
        "message": null,
        "error": null,
        "started_at": at.filter(|_| state != "skipped").map(time),
        "finished_at": at.filter(|_| done && state != "skipped").map(time),
    })
}

/// The stages of a new preview: the cheap ones ran before the answer.
pub fn initial(now: OffsetDateTime) -> Value {
    Value::Array(vec![
        stage("unpack", "done", Some(now)),
        stage("validate", "done", Some(now)),
        stage("build", "skipped", None),
        stage("test", "skipped", None),
        // No binding before card B-1: the stage has nothing to do (P-62: `skipped`).
        stage("bindings", "skipped", None),
        stage("image", "pending", None),
        stage("start", "pending", None),
        stage("check", "pending", None),
    ])
}

/// The stages of a release (a publish or a rollback): every stage ran before the switch.
pub fn release(now: OffsetDateTime) -> Value {
    Value::Array(vec![
        stage("unpack", "done", Some(now)),
        stage("validate", "done", Some(now)),
        stage("build", "skipped", None),
        stage("test", "skipped", None),
        stage("bindings", "skipped", None),
        stage("image", "done", Some(now)),
        stage("start", "done", Some(now)),
        stage("check", "done", Some(now)),
        stage("switch", "done", Some(now)),
    ])
}

/// Sets the state of stage `name`; `failed` also sets its message and machine-readable error.
fn set(stages: &mut Value, name: &str, state: &str, err: Option<&StageError>) {
    let now = crate::now();
    let Some(list) = stages.as_array_mut() else {
        return;
    };
    for s in list.iter_mut() {
        if s["name"] != name {
            continue;
        }
        s["state"] = json!(state);
        if state == "running" {
            s["started_at"] = time(now);
        }
        if matches!(state, "done" | "failed") {
            if s["started_at"].is_null() {
                s["started_at"] = time(now);
            }
            s["finished_at"] = time(now);
        }
        if let Some(e) = err {
            s["message"] = json!(e.message);
            s["error"] = json!({
                "code": e.code.as_str(),
                "hint": e.hint,
                "details": e.details,
            });
        }
    }
}

/// A failed stage: the stage, a snake_case reason (`app.deployment_failed.reason`), the error
/// of the stage (`AppStageError`) and a message for callers with `edit`.
#[derive(Clone, Debug)]
pub struct StageError {
    pub stage: &'static str,
    pub reason: &'static str,
    pub code: ErrorCode,
    pub hint: Option<String>,
    pub message: String,
    pub details: Value,
}

impl StageError {
    fn new(stage: &'static str, reason: &'static str, message: impl Into<String>) -> Self {
        Self {
            stage,
            reason,
            code: ErrorCode::Internal,
            hint: None,
            message: message.into(),
            details: json!({}),
        }
    }
}

/// The work of one preview after the `202`.
pub(crate) struct Run {
    pub id: Uuid,
    pub app: NodeId,
    pub manifest: Value,
    pub files: Vec<(String, bytes::Bytes)>,
    pub stages: Value,
}

/// The spec of a static image: a service that runs no code (R-1's rules).
fn static_spec(identity: WorkloadIdentity, image: StaticImage) -> WorkloadSpec {
    WorkloadSpec {
        identity,
        kind: WorkloadKind::Service(ServiceSpec {
            port: 8080,
            health: None,
            idle_timeout: None,
        }),
        image: ImageRef::Static(image),
        command: Vec::new(),
        env: Vec::new(),
        previous_secrets: Vec::new(),
        // A static instance runs no code, so the token reaches nothing; it is random anyway.
        workload_token: SecretValue::new(hex::encode(rand::random::<[u8; 32]>())),
        limits: Limits {
            cpu_millis: 100,
            memory_bytes: 16 * 1024 * 1024,
            disk_bytes: 16 * 1024 * 1024,
        },
        routes: Vec::new(),
    }
}

/// Applies a static image through the runtime driver (the stage `start`): the driver checks the
/// image and reports a running instance; the instance is then stopped, because the gateway
/// serves the files from the site index and no code runs.
pub async fn start_static(
    driver: &dyn RuntimeDriver,
    tenant: TenantId,
    app: NodeId,
    deployment: Uuid,
    environment: runtime::Environment,
    image: StaticImage,
) -> Result<(), StageError> {
    let identity = WorkloadIdentity {
        tenant,
        app,
        deployment,
        environment,
    };
    let fail = |e: runtime::RuntimeError| {
        let mut s = StageError::new(
            "start",
            "runtime_refused",
            format!("the runtime refused the static image: {e}"),
        );
        s.details = json!({ "runtime_error": e.code() });
        s
    };
    let status = driver
        .start(static_spec(identity, image))
        .await
        .map_err(fail)?;
    let mut state = status.state;
    for _ in 0..50 {
        if state != InstanceState::Starting {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        state = driver.status(tenant, status.id).await.map_err(fail)?.state;
    }
    let running = state == InstanceState::Running;
    if let Err(e) = driver.stop(tenant, status.id).await {
        tracing::warn!(%tenant, error = %e, "could not stop a static instance");
    }
    if running {
        Ok(())
    } else {
        Err(StageError::new(
            "start",
            "not_running",
            format!("the static instance did not start (state {})", state.name()),
        ))
    }
}

/// The result of the static check: `/` and every route of `check.routes` resolve in the
/// site index (the browser check is card O-4).
#[derive(Clone, Debug)]
pub struct CheckOutcome {
    pub passed: bool,
    /// `AppCheckResult`.
    pub result: Value,
}

/// The static check of a site (see [`CheckOutcome`]).
pub fn check_site(index: &SiteIndex, manifest: &Value) -> CheckOutcome {
    let mut routes: Vec<String> = vec!["/".into()];
    if let Some(list) = manifest.pointer("/check/routes").and_then(Value::as_array) {
        for r in list.iter().filter_map(Value::as_str) {
            if !routes.iter().any(|x| x == r) {
                routes.push(r.to_string());
            }
        }
    }
    let mut details = Vec::new();
    for r in &routes {
        let path = r.split(['?', '#']).next().unwrap_or_default();
        let found = super::super::static_site::request_path(path)
            .and_then(|p| index.resolve(p, true))
            .is_some();
        if !found && details.len() < 50 {
            details.push(json!({
                "route": r,
                "kind": "failed_request",
                "message": "the site has no file for this route (index.html, spa_fallback or the file itself)",
            }));
        }
    }
    let failed = details.len();
    CheckOutcome {
        passed: failed == 0,
        result: json!({
            "passed": failed == 0,
            "routes": routes.len(),
            "console_errors": 0,
            "failed_requests": failed,
            "server_errors": 0,
            "new_issue_ids": [],
            "details": details,
        }),
    }
}

impl AppService {
    /// The stages after the `202` of a preview. Every failure ends in a `failed` deployment
    /// with `app.deployment_failed`; nothing here answers a request.
    pub(crate) async fn run_stages(self: Arc<Self>, ctx: Context, mut run: Run) {
        let outcome = self.stages(&ctx, &mut run).await;
        let stored = match outcome {
            Ok(check) => self
                .deps
                .store
                .finish_preview(&ctx, run.id, &run.stages, Some(&check))
                .await
                .map(|_| ()),
            Err(failed) => {
                let (e, check) = *failed;
                set(&mut run.stages, e.stage, "failed", Some(&e));
                for later in STAGE_NAMES.iter().skip_while(|s| **s != e.stage).skip(1) {
                    set(&mut run.stages, later, "skipped", None);
                }
                self.deps
                    .store
                    .fail_deployment(&ctx, run.id, &run.stages, check.as_ref(), e.stage, e.reason)
                    .await
            }
        };
        if let Err(e) = stored {
            tracing::warn!(tenant = %ctx.tenant, deployment = %run.id, error = %e,
                "could not store the end of a deployment");
        }
        self.notify_changed();
    }

    async fn progress(&self, ctx: &Context, run: &Run) {
        if let Err(e) = self
            .deps
            .store
            .update_stages(ctx.tenant, run.id, &run.stages)
            .await
        {
            tracing::warn!(tenant = %ctx.tenant, deployment = %run.id, error = %e,
                "could not store the stages of a deployment");
        }
        self.notify_changed();
    }

    /// `image`, `start`, `check`. The check result on success; the failed stage (and the check
    /// result, when the check ran) otherwise.
    async fn stages(
        &self,
        ctx: &Context,
        run: &mut Run,
    ) -> Result<Value, Box<(StageError, Option<Value>)>> {
        let tenant = ctx.tenant;
        // image
        set(&mut run.stages, "image", "running", None);
        self.progress(ctx, run).await;
        if run.files.is_empty() {
            let mut e = StageError::new(
                "image",
                "static_root_empty",
                "the static root of the archive holds no files",
            );
            e.code = ErrorCode::ValidationFailed;
            e.hint = Some("/static/root".into());
            e.details = json!({ "pointer": "/static/root" });
            return Err(Box::new((e, None)));
        }
        let spa = run
            .manifest
            .pointer("/static/spa_fallback")
            .and_then(Value::as_str)
            .map(|s| s.trim_start_matches("./").to_string());
        let written = self
            .deps
            .sites
            .write_site(tenant, run.id, &run.files, spa)
            .await
            .map_err(|err| {
                tracing::warn!(%tenant, deployment = %run.id, error = %err, "the image stage failed");
                Box::new((
                    StageError::new("image", "image_failed", "the static files could not be stored"),
                    None,
                ))
            })?;
        run.files.clear();
        if written.new_bytes > 0 {
            let event = UsageEvent {
                tenant,
                app: run.app,
                deployment: Some(run.id),
                charged: Charged::App,
                meter: Meter::StorageBytes,
                quantity: i64::try_from(written.new_bytes).unwrap_or(i64::MAX),
                at: crate::now(),
            };
            if let Err(e) = self.deps.usage.record(event).await {
                tracing::warn!(%tenant, error = %e, "could not record the storage meter");
            }
        }
        set(&mut run.stages, "image", "done", None);
        // start
        set(&mut run.stages, "start", "running", None);
        self.progress(ctx, run).await;
        start_static(
            &*self.deps.runtime,
            tenant,
            run.app,
            run.id,
            runtime::Environment::Preview,
            written.image.clone(),
        )
        .await
        .map_err(|e| Box::new((e, None)))?;
        set(&mut run.stages, "start", "done", None);
        // check
        set(&mut run.stages, "check", "running", None);
        self.progress(ctx, run).await;
        let index = match self.deps.sites.read_index(tenant, run.id).await {
            Ok(Some(i)) if i == written.index => i,
            _ => {
                return Err(Box::new((
                    StageError::new(
                        "check",
                        "index_unreadable",
                        "the site index does not read back",
                    ),
                    None,
                )));
            }
        };
        let check = check_site(&index, &run.manifest);
        if !check.passed {
            let mut e = StageError::new(
                "check",
                "check_failed",
                format!(
                    "{} route(s) of the check have no file in the site (see check.details)",
                    check.result["failed_requests"]
                ),
            );
            e.code = ErrorCode::ValidationFailed;
            e.hint = Some("/check/routes".into());
            e.details = json!({ "pointer": "/check/routes" });
            return Err(Box::new((e, Some(check.result))));
        }
        set(&mut run.stages, "check", "done", None);
        Ok(check.result)
    }
}
