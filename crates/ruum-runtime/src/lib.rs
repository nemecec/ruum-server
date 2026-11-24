//! The Docker runtime driver (card R-2a): [`DockerDriver`] implements
//! [`ruum_core::runtime::RuntimeDriver`] on a Docker engine, with plain `runc` or gVisor
//! (`runsc`) as the OCI runtime ([`RuntimeKind`], `RUUM_RUNTIME_DRIVER=docker|runsc`). It passes
//! the conformance suite of `ruum_core::runtime::conformance` unchanged
//! (`tests/conformance.rs`), and the hostile sandbox suite (`tests/sandbox.rs`) checks the
//! sandbox from inside.
//!
//! # The sandbox (STEP3 "How an app runs" rule 1; decision 0006 G point 1)
//!
//! Every container gets: a read-only root file system; `cap-drop ALL`; `no-new-privileges`; the
//! user `10001:10001` (the image's `USER` is ignored); Docker's default seccomp and AppArmor
//! profiles (never `unconfined`); CPU, memory (swap = memory) and process limits; a size-limited
//! `tmpfs` at `/tmp` (the spec's disk limit); no Docker socket and no bind mount but the
//! workload token file (read-only); no `--privileged`, no host network, PID, IPC or UTS
//! namespace; no published ports; `init`; a bounded stop timeout; the log driver `none` (the
//! output reaches only the driver, never the engine's disk). After create, the driver reads the
//! container back and refuses it when a flag or a limit differs ([`Applied::limits`] is what the
//! engine applied).
//!
//! [`Applied::limits`]: ruum_core::runtime::Applied::limits
//!
//! # Networks (rule 2)
//!
//! One network per app and environment ([`network_name`]): `internal: true`, created with
//! `com.docker.network.bridge.inhibit_ipv4=true` (no host address on the bridge), IPv6 off,
//! labelled `ruum.app`, `ruum.tenant`, `ruum.env`. The shared containers of the spec's routes
//! (from [`DockerConfig::routes`], never from the spec) join it with their alias; the egress
//! proxy's registration goes through [`EgressRegistry`]. A spec without routes gets a network
//! with nothing else on it. The last instance to leave removes the network. The host cannot
//! reach an app, so health is Docker's own `HEALTHCHECK`, run inside the sandbox.
//!
//! # Images, token, secrets, logs
//!
//! An `oci` digest is checked at the registry in the repository of the spec's tenant and app
//! ([`repository`]) before anything else, so the digest of another app's image is
//! `image_not_found` also when the engine has it. The workload token is a host file in a
//! private (0700) folder per instance, bind-mounted read-only at `TOKEN_FILE` and rewritten in
//! place by `rotate_token`. Secret values reach the container at create only, as environment
//! values; they are never in a label, a log line, an error text or `Debug` output (errors from
//! the engine are redacted). Output streams through the redactor of `ruum-core` inside the
//! driver into a bounded ring per instance; a line is cut at [`DockerConfig::max_line_bytes`].
//!
//! # Docker API surface (for I-4's socket proxy)
//!
//! The crate README lists every call the driver makes and the labels it sets.
//!
//! # The Kubernetes driver (backlog I6-1)
//!
//! [`KubeDriver`] ([`kube`]) is the same trait on Kubernetes: one gVisor pod per run in the
//! namespace of the sandboxes (`ruum-apps` on staging), with the same spec, limits and log
//! redaction. The README ("Kubernetes driver") lists its API calls and its Role.

pub mod builder;
mod config;
mod driver;
mod egress;
pub mod kube;
mod logs;

pub use config::{DEFAULT_HEALTH_COMMAND, DockerConfig, RuntimeKind, SharedContainer};
pub use driver::{
    DockerDriver, LABEL_APP, LABEL_DEPLOYMENT, LABEL_ENV, LABEL_INSTANCE, LABEL_MANAGED,
    LABEL_TENANT, RESERVED_ENV, SANDBOX_USER, label_keys, network_name, repository,
};
pub use egress::{EgressNetwork, EgressRegistry, HttpEgressRegistry, NoEgress};

pub use builder::{BuilderConfig, DockerBuilder, RegistryCredential};
pub use kube::{KubeConfig, KubeDriver, SharedService};
