//! The build job (card R-2b, "The build job"): one sandbox container per build that runs the
//! fixed `node22` recipe (decision 0006 G point 5), on a network of its own.
//!
//! The container has the hardening of an app instance of the Docker driver: read-only root,
//! `cap-drop ALL`, `no-new-privileges`, user `10001:10001`, the pids limit of the driver's
//! configuration, the CPU and memory limits of the build (swap = memory), `init`, private IPC,
//! no published ports, Docker's default seccomp and AppArmor profiles, the same OCI runtime
//! (`runc` or `runsc`), the log driver `none`. Its only writable folders are two size-limited
//! `tmpfs` mounts: `/work` (the source and its `node_modules`, `exec` allowed for the build's
//! own tools) and `/tmp` (`noexec`; npm's cache), each of `BuildInput::limits.disk_bytes`.
//! No bind mount, no volume, no Docker socket.
//!
//! The network is created for the build only: `internal: true`, `inhibit_ipv4` (decision 0006
//! G point 1), IPv6 off; the egress proxy is the only other container on it, and the network is
//! registered at the proxy with the build's identity and the npm registry hosts as its allow
//! list. The job reaches the proxy by an `/etc/hosts` line (runsc has no embedded DNS).
//!
//! The source goes in on standard input (a tar the builder wrote itself from the checked
//! archive, read with `head -c <size>` so the job never waits for an end of stream); the log
//! comes out on standard error and the output tar on standard output (the script moves npm's
//! own output to standard error). Nothing of the app is ever part of the script: the optional
//! `build` command of the manifest reaches the job as the variable `RUUM_BUILD_COMMAND`.

use super::BuilderConfig;
use crate::driver::{
    LABEL_APP, LABEL_DEPLOYMENT, LABEL_ENV, LABEL_MANAGED, LABEL_TENANT, SANDBOX_USER,
};
use crate::egress::{EgressNetwork, EgressRegistry};
use bollard::Docker;
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, EndpointSettings, HealthConfig, HostConfig,
    HostConfigLogConfig, NetworkConnectRequest, NetworkCreateRequest, NetworkDisconnectRequest,
};
use bollard::query_parameters::{
    AttachContainerOptionsBuilder, CreateContainerOptionsBuilder, KillContainerOptionsBuilder,
    RemoveContainerOptionsBuilder,
};
use futures::StreamExt;
use ruum_core::runtime::{BuildInput, Limits, Route, RuntimeError};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// The label of everything that belongs to one build: its ID.
pub const LABEL_BUILD: &str = "ruum.build";
/// The value of [`LABEL_MANAGED`] on the builder's objects.
pub const MANAGED_BY: &str = "ruum-builder";

const INHIBIT_IPV4: &str = "com.docker.network.bridge.inhibit_ipv4";

/// The fixed recipe. `$RUUM_SOURCE_BYTES` and `$RUUM_BUILD_COMMAND` are set by the builder;
/// no value of the app is in this text.
pub const RECIPE: &str = r#"set -eu
exec 3>&1 1>&2
mkdir /work/app
head -c "$RUUM_SOURCE_BYTES" | tar -x -f - -C /work/app
cd /work/app
echo "ruum-build: npm ci"
npm ci --ignore-scripts=false --foreground-scripts --no-audit --no-fund
if [ -n "${RUUM_BUILD_COMMAND:-}" ]; then
  echo "ruum-build: the build command"
  sh -c "$RUUM_BUILD_COMMAND"
fi
echo "ruum-build: output"
tar -c -f - -C /work/app . >&3
"#;

/// The names of one build's container and network.
pub fn container_name(build: uuid::Uuid) -> String {
    format!("ruum-b-{}", build.simple())
}

pub fn network_name(build: uuid::Uuid) -> String {
    format!("ruum-build-{}", build.simple())
}

/// The labels of a build's network and container.
pub fn labels(
    cfg: &BuilderConfig,
    input: &BuildInput,
    build: uuid::Uuid,
) -> HashMap<String, String> {
    let mut l: HashMap<String, String> = cfg
        .docker
        .extra_labels
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    l.insert(LABEL_APP.to_owned(), input.identity.app.0.to_string());
    l.insert(LABEL_TENANT.to_owned(), input.identity.tenant.0.to_string());
    l.insert(
        LABEL_ENV.to_owned(),
        input.identity.environment.as_str().to_owned(),
    );
    l.insert(
        LABEL_DEPLOYMENT.to_owned(),
        input.identity.deployment.to_string(),
    );
    l.insert(LABEL_BUILD.to_owned(), build.to_string());
    l.insert(LABEL_MANAGED.to_owned(), MANAGED_BY.to_owned());
    l
}

pub fn status_of(e: &bollard::errors::Error) -> Option<u16> {
    match e {
        bollard::errors::Error::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}

fn engine(what: &str, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Unavailable {
        reason: format!("{what}: {e}"),
    }
}

/// The network of a build, made and registered: its ID and the `/etc/hosts` line of the
/// egress proxy.
pub struct BuildNetwork {
    pub id: String,
    pub hosts: Vec<String>,
}

/// Creates the build's network, attaches the egress proxy and registers the network at it.
/// The caller records the ID before registration fails or the build is cancelled, so the
/// cleanup finds it ([`remove_network`] takes the name).
pub async fn create_network(
    docker: &Docker,
    cfg: &BuilderConfig,
    egress: &dyn EgressRegistry,
    input: &BuildInput,
    build: uuid::Uuid,
    registered: &std::sync::Mutex<Option<String>>,
) -> Result<BuildNetwork, RuntimeError> {
    let name = network_name(build);
    let proxy =
        cfg.docker
            .routes
            .get(&Route::EgressProxy)
            .ok_or_else(|| RuntimeError::Internal {
                reason: "the builder has no egress proxy (DockerConfig::routes)".to_owned(),
            })?;
    let req = NetworkCreateRequest {
        name: name.clone(),
        driver: Some("bridge".to_owned()),
        internal: Some(true),
        attachable: Some(false),
        enable_ipv6: Some(false),
        options: Some(HashMap::from([(
            INHIBIT_IPV4.to_owned(),
            "true".to_owned(),
        )])),
        labels: Some(labels(cfg, input, build)),
        ..Default::default()
    };
    let created = docker
        .create_network(req)
        .await
        .map_err(|e| engine("creating the build network", e))?;
    let id = created.id;
    let inspect = |docker: &Docker| {
        let name = name.clone();
        let docker = docker.clone();
        async move {
            docker
                .inspect_network(
                    &name,
                    None::<bollard::query_parameters::InspectNetworkOptions>,
                )
                .await
                .map_err(|e| engine("inspecting the build network", e))
        }
    };
    let n = inspect(docker).await?;
    let inhibit = n
        .options
        .as_ref()
        .and_then(|o| o.get(INHIBIT_IPV4))
        .map(String::as_str);
    if n.internal != Some(true) || inhibit != Some("true") || n.enable_ipv6 == Some(true) {
        return Err(RuntimeError::Internal {
            reason: format!(
                "the engine made the build network with internal={:?}, inhibit_ipv4={inhibit:?}, ipv6={:?}",
                n.internal, n.enable_ipv6
            ),
        });
    }
    let subnet = n
        .ipam
        .as_ref()
        .and_then(|i| i.config.as_ref())
        .and_then(|c| c.iter().find_map(|x| x.subnet.clone()))
        .ok_or_else(|| RuntimeError::Internal {
            reason: "the build network has no subnet".to_owned(),
        })?;
    docker
        .connect_network(
            &id,
            NetworkConnectRequest {
                container: proxy.container.clone(),
                endpoint_config: Some(EndpointSettings {
                    aliases: Some(vec![proxy.alias.clone()]),
                    ..Default::default()
                }),
            },
        )
        .await
        .map_err(|e| engine("attaching the egress proxy", e))?;
    let n = inspect(docker).await?;
    let want = proxy.container.trim_start_matches('/');
    let containers = n.containers.unwrap_or_default();
    if containers.len() != 1 {
        return Err(RuntimeError::Internal {
            reason: format!(
                "the build network holds {} containers before the job, not the egress proxy only",
                containers.len()
            ),
        });
    }
    let ip = containers
        .iter()
        .find(|(id, c)| {
            id.as_str() == want
                || id.starts_with(want)
                || c.name.as_deref().map(|n| n.trim_start_matches('/')) == Some(want)
        })
        .and_then(|(_, c)| c.ipv4_address.as_deref())
        .and_then(|a| a.split('/').next())
        .and_then(|a| a.parse::<std::net::Ipv4Addr>().ok())
        .ok_or_else(|| RuntimeError::Internal {
            reason: "the egress proxy has no address on the build network".to_owned(),
        })?;
    *registered.lock().unwrap_or_else(|p| p.into_inner()) = Some(id.clone());
    egress
        .register(&EgressNetwork {
            network: id.clone(),
            subnet,
            identity: input.identity,
            allow: Some(cfg.npm_hosts.clone()),
        })
        .await?;
    Ok(BuildNetwork {
        id,
        hosts: vec![format!("{}:{ip}", proxy.alias)],
    })
}

/// Unregisters the network at the egress proxy (three tries), detaches the proxy and removes
/// the network. A network whose unregistration keeps failing is left (its subnet stays taken,
/// so the proxy never sees an overlapping one); the next prune removes it.
pub async fn remove_network(
    docker: &Docker,
    cfg: &BuilderConfig,
    egress: &dyn EgressRegistry,
    build: uuid::Uuid,
    registered: Option<String>,
) {
    let name = network_name(build);
    if let Some(id) = &registered {
        let mut done = false;
        for attempt in 0..3u32 {
            match egress.unregister(id).await {
                Ok(()) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(network = %name, attempt, error = %e, "unregistering a build network");
                    tokio::time::sleep(Duration::from_millis(250 << attempt)).await;
                }
            }
        }
        if !done {
            return;
        }
    }
    if let Some(p) = cfg.docker.routes.get(&Route::EgressProxy) {
        let _ = docker
            .disconnect_network(
                &name,
                NetworkDisconnectRequest {
                    container: p.container.clone(),
                    force: Some(true),
                },
            )
            .await;
    }
    if let Err(e) = docker.remove_network(&name).await
        && status_of(&e) != Some(404)
    {
        tracing::warn!(network = %name, error = %e, "removing a build network");
    }
}

/// The variables of the job: a fixed list (no value of the builder's own environment).
pub fn job_env(
    cfg: &BuilderConfig,
    source_bytes: usize,
    build_command: Option<&str>,
) -> Vec<String> {
    let mut env = vec![
        "HOME=/tmp".to_owned(),
        "npm_config_cache=/tmp/.npm".to_owned(),
        "npm_config_update_notifier=false".to_owned(),
        "npm_config_fund=false".to_owned(),
        "npm_config_audit=false".to_owned(),
        format!("npm_config_registry={}", cfg.npm_registry),
        format!("RUUM_SOURCE_BYTES={source_bytes}"),
    ];
    if let Some(p) = cfg.docker.routes.get(&Route::EgressProxy) {
        let proxy = format!("http://{}:{}", p.alias, p.port);
        for name in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            env.push(format!("{name}={proxy}"));
        }
        env.push("NO_PROXY=localhost,127.0.0.1".to_owned());
        env.push("no_proxy=localhost,127.0.0.1".to_owned());
        env.push("NODE_USE_ENV_PROXY=1".to_owned());
    }
    if let Some(b) = build_command {
        env.push(format!("RUUM_BUILD_COMMAND={b}"));
    }
    env
}

fn tmpfs_options(disk: u64, exec: bool) -> String {
    format!(
        "rw,{},nosuid,nodev,size={disk},mode=1777",
        if exec { "exec" } else { "noexec" }
    )
}

/// The create body of the job container.
pub fn container_body(
    cfg: &BuilderConfig,
    input: &BuildInput,
    build: uuid::Uuid,
    image: &str,
    network: &BuildNetwork,
    env: Vec<String>,
) -> ContainerCreateBody {
    let disk = input.limits.disk_bytes;
    let host = HostConfig {
        readonly_rootfs: Some(true),
        cap_drop: Some(vec!["ALL".to_owned()]),
        security_opt: Some(vec!["no-new-privileges".to_owned()]),
        privileged: Some(false),
        memory: Some(input.limits.memory_bytes as i64),
        memory_swap: Some(input.limits.memory_bytes as i64),
        nano_cpus: Some(i64::from(input.limits.cpu_millis) * 1_000_000),
        pids_limit: Some(cfg.docker.pids_limit),
        tmpfs: Some(HashMap::from([
            ("/work".to_owned(), tmpfs_options(disk, true)),
            ("/tmp".to_owned(), tmpfs_options(disk, false)),
        ])),
        network_mode: Some(network_name(build)),
        extra_hosts: Some(network.hosts.clone()),
        ipc_mode: Some("private".to_owned()),
        init: Some(true),
        publish_all_ports: Some(false),
        log_config: Some(HostConfigLogConfig {
            typ: Some("none".to_owned()),
            config: None,
        }),
        runtime: Some(cfg.docker.runtime.docker_name().to_owned()),
        ..Default::default()
    };
    ContainerCreateBody {
        image: Some(image.to_owned()),
        entrypoint: Some(vec!["/bin/sh".to_owned(), "-c".to_owned()]),
        cmd: Some(vec![RECIPE.to_owned()]),
        env: Some(env),
        user: Some(SANDBOX_USER.to_owned()),
        working_dir: Some("/work".to_owned()),
        labels: Some(labels(cfg, input, build)),
        healthcheck: Some(HealthConfig {
            test: Some(vec!["NONE".to_owned()]),
            ..Default::default()
        }),
        stop_timeout: Some(1),
        attach_stdin: Some(true),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        open_stdin: Some(true),
        stdin_once: Some(true),
        tty: Some(false),
        network_disabled: Some(false),
        host_config: Some(host),
        ..Default::default()
    }
}

/// Reads the job container back from the engine: every flag as asked (a mismatch is an
/// error, the build does not start).
pub fn verify(
    c: &ContainerInspectResponse,
    cfg: &BuilderConfig,
    limits: &Limits,
    build: uuid::Uuid,
    hosts: &[String],
) -> Result<(), String> {
    let h = c.host_config.as_ref().ok_or("no host config")?;
    let conf = c.config.as_ref().ok_or("no config")?;
    let mut bad = Vec::new();
    let mut want = |ok: bool, what: &str| {
        if !ok {
            bad.push(what.to_owned());
        }
    };
    want(h.readonly_rootfs == Some(true), "read-only root");
    want(
        h.cap_drop.as_deref() == Some(&["ALL".to_owned()][..]),
        "cap-drop ALL",
    );
    want(
        h.cap_add.as_ref().is_none_or(|c| c.is_empty()),
        "no added capability",
    );
    let sec = h.security_opt.clone().unwrap_or_default();
    want(
        sec.iter()
            .any(|s| s == "no-new-privileges" || s == "no-new-privileges:true"),
        "no-new-privileges",
    );
    want(
        !sec.iter().any(|s| s.contains("unconfined")),
        "no unconfined profile",
    );
    want(h.privileged != Some(true), "not privileged");
    want(
        h.network_mode.as_deref() == Some(network_name(build).as_str()),
        "the build network",
    );
    want(
        h.pid_mode.as_deref().unwrap_or("").is_empty(),
        "own PID namespace",
    );
    want(
        h.uts_mode.as_deref().unwrap_or("").is_empty(),
        "own UTS namespace",
    );
    want(
        h.userns_mode.as_deref().unwrap_or("").is_empty(),
        "no host user namespace",
    );
    want(h.ipc_mode.as_deref() == Some("private"), "private IPC");
    want(
        h.extra_hosts.as_deref().unwrap_or_default() == hosts,
        "only the egress proxy in /etc/hosts",
    );
    want(h.init == Some(true), "init");
    want(h.publish_all_ports != Some(true), "no published ports");
    want(
        h.port_bindings.as_ref().is_none_or(|p| p.is_empty()),
        "no port bindings",
    );
    want(
        h.devices.as_ref().is_none_or(|d| d.is_empty()),
        "no devices",
    );
    want(h.mounts.as_ref().is_none_or(|m| m.is_empty()), "no mounts");
    want(h.binds.as_ref().is_none_or(|b| b.is_empty()), "no binds");
    want(
        c.mounts.as_ref().is_none_or(|m| m.is_empty()),
        "nothing mounted (no volume)",
    );
    want(h.pids_limit == Some(cfg.docker.pids_limit), "pids limit");
    want(h.memory_swap == h.memory, "swap equals memory");
    want(
        h.runtime.as_deref() == Some(cfg.docker.runtime.docker_name()),
        "the runtime",
    );
    want(conf.user.as_deref() == Some(SANDBOX_USER), "user 10001");
    want(
        c.app_armor_profile.as_deref() != Some("unconfined"),
        "no AppArmor unconfined",
    );
    want(
        h.nano_cpus == Some(i64::from(limits.cpu_millis) * 1_000_000),
        "the CPU limit",
    );
    want(
        h.memory == Some(limits.memory_bytes as i64),
        "the memory limit",
    );
    let tmpfs = h.tmpfs.clone().unwrap_or_default();
    want(
        tmpfs.len() == 2
            && tmpfs.get("/work") == Some(&tmpfs_options(limits.disk_bytes, true))
            && tmpfs.get("/tmp") == Some(&tmpfs_options(limits.disk_bytes, false)),
        "the size-limited work and tmp folders",
    );
    if bad.is_empty() {
        Ok(())
    } else {
        Err(bad.join(", "))
    }
}

/// How a job ended.
pub enum JobEnd {
    /// The process ended with this code; its output (standard output) is complete.
    Exited { code: i64, output: bytes::Bytes },
    /// The output went over the cap: the job was killed.
    OutputOverCap,
}

/// The bounded tail of the build log: whole lines, each cut at `max_line` bytes.
pub struct LogTail {
    lines: std::collections::VecDeque<(time::OffsetDateTime, String)>,
    partial: Vec<u8>,
    max_lines: usize,
    max_line: usize,
}

impl LogTail {
    pub fn new(max_lines: usize, max_line: usize) -> Self {
        Self {
            lines: std::collections::VecDeque::new(),
            partial: Vec::new(),
            max_lines,
            max_line,
        }
    }

    fn push_line(&mut self, mut line: Vec<u8>) {
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        line.truncate(self.max_line);
        let text = String::from_utf8_lossy(&line).into_owned();
        if self.lines.len() == self.max_lines {
            self.lines.pop_front();
        }
        self.lines
            .push_back((time::OffsetDateTime::now_utc(), text));
    }

    pub fn push_bytes(&mut self, data: &[u8]) {
        for b in data {
            if *b == b'\n' {
                let line = std::mem::take(&mut self.partial);
                self.push_line(line);
            } else if self.partial.len() < self.max_line {
                self.partial.push(*b);
            }
        }
    }

    pub fn note(&mut self, text: &str) {
        self.push_line(text.as_bytes().to_vec());
    }

    pub fn finish(&mut self) {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.push_line(line);
        }
    }

    pub fn lines(&self) -> Vec<(time::OffsetDateTime, String)> {
        self.lines.iter().cloned().collect()
    }
}

/// Creates, checks and runs the job container to its end. The caller removes the container
/// (by [`container_name`]) after every outcome, also when this future is dropped at the build's
/// timeout.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    docker: &Docker,
    cfg: &BuilderConfig,
    input: &BuildInput,
    build: uuid::Uuid,
    image: &str,
    network: &BuildNetwork,
    source: bytes::Bytes,
    build_command: Option<&str>,
    log: &std::sync::Mutex<LogTail>,
) -> Result<JobEnd, RuntimeError> {
    let name = container_name(build);
    let env = job_env(cfg, source.len(), build_command);
    let body = container_body(cfg, input, build, image, network, env);
    let opts = CreateContainerOptionsBuilder::new().name(&name).build();
    docker
        .create_container(Some(opts), body)
        .await
        .map_err(|e| engine("creating the build job", e))?;
    let c = docker
        .inspect_container(
            &name,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await
        .map_err(|e| engine("inspecting the build job", e))?;
    verify(&c, cfg, &input.limits, build, &network.hosts).map_err(|bad| {
        RuntimeError::Internal {
            reason: format!("the engine did not apply the build job's sandbox: {bad}"),
        }
    })?;
    let opts = AttachContainerOptionsBuilder::new()
        .stream(true)
        .stdin(true)
        .stdout(true)
        .stderr(true)
        .logs(false)
        .build();
    let attached = docker
        .attach_container(&name, Some(opts))
        .await
        .map_err(|e| engine("attaching to the build job", e))?;
    docker
        .start_container(&name, None)
        .await
        .map_err(|e| engine("starting the build job", e))?;
    let mut input_w = attached.input;
    let writer = tokio::spawn(async move {
        // A job that stops reading (it failed early) ends the write with an error: fine.
        let _ = input_w.write_all(&source).await;
        let _ = input_w.flush().await;
        let _ = input_w.shutdown().await;
    });
    let cap = cfg.output_max;
    let mut out: Vec<u8> = Vec::new();
    let mut over = false;
    let mut stream = attached.output;
    while let Some(item) = stream.next().await {
        let Ok(item) = item else { break };
        match item {
            bollard::container::LogOutput::StdOut { message } => {
                if over {
                    continue;
                }
                if out.len() as u64 + message.len() as u64 > cap {
                    over = true;
                    let k = KillContainerOptionsBuilder::new().signal("SIGKILL").build();
                    let _ = docker.kill_container(&name, Some(k)).await;
                    continue;
                }
                out.extend_from_slice(&message);
            }
            bollard::container::LogOutput::StdErr { message }
            | bollard::container::LogOutput::Console { message } => {
                log.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push_bytes(&message);
            }
            bollard::container::LogOutput::StdIn { .. } => {}
        }
    }
    writer.abort();
    log.lock().unwrap_or_else(|p| p.into_inner()).finish();
    if over {
        return Ok(JobEnd::OutputOverCap);
    }
    let code = exit_code(docker, &name).await?;
    Ok(JobEnd::Exited {
        code,
        output: bytes::Bytes::from(out),
    })
}

/// The exit code of an ended container (waits for its end).
async fn exit_code(docker: &Docker, name: &str) -> Result<i64, RuntimeError> {
    let mut wait = docker.wait_container(
        name,
        None::<bollard::query_parameters::WaitContainerOptions>,
    );
    match wait.next().await {
        Some(Ok(r)) => Ok(r.status_code),
        Some(Err(bollard::errors::Error::DockerContainerWaitError { code, .. })) => Ok(code),
        Some(Err(e)) => Err(engine("waiting for the build job", e)),
        None => Err(engine("waiting for the build job", "no answer")),
    }
}

/// Removes the job container (by force: a running one is killed).
pub async fn remove_container(docker: &Docker, build: uuid::Uuid) {
    let name = container_name(build);
    let opts = RemoveContainerOptionsBuilder::new()
        .force(true)
        .v(true)
        .build();
    if let Err(e) = docker.remove_container(&name, Some(opts)).await
        && status_of(&e) != Some(404)
    {
        tracing::warn!(container = %name, error = %e, "removing a build job");
    }
}
