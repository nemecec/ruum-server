//! The `Builder` trait (module border, design 18.1): turns an app archive into an OCI image in
//! the local registry. Card R-2b implements it with Railpack inside the sandbox.
//!
//! Builds are untrusted code (rule 1: `npm install` runs postinstall scripts). A builder runs
//! the build under the same rules as an app: the [`Limits`] of the input, the timeout, and only
//! the egress proxy as a route ([`BUILD_ROUTES`]). **A build gets no secrets and no bindings**
//! (rule 7): [`BuildInput`] has no environment, no token and no routes, so it cannot carry them.
//! A test destructures it field by field, so a new field needs a review of that rule.
//!
//! The type is not enough on its own: a builder must also not let its own environment or its
//! own credentials (the registry push credential) reach the build. The conformance case
//! `build_sees_no_platform_secrets` checks this with values the fixture plants
//! ([`super::conformance::BuilderFixture::canaries`]).

use super::{ImageRef, Limits, Route, RuntimeError, WorkloadIdentity};
use async_trait::async_trait;
use bytes::Bytes;
use std::time::Duration;
use time::OffsetDateTime;

/// The only routes a build gets: the egress proxy (for package downloads).
pub const BUILD_ROUTES: &[Route] = &[Route::EgressProxy];

/// The most lines a builder returns in [`BuildOutput::logs`]: the end of the build log. A
/// builder keeps a bounded tail while the build runs, never the whole output of an untrusted
/// build.
pub const MAX_BUILD_LOG_LINES: usize = 1000;

/// The longest [`RuntimeError::BuildFailed`] reason a builder returns, in bytes.
pub const MAX_BUILD_FAILED_REASON_BYTES: usize = 4096;

/// What a build gets. Nothing else: no secrets, no bindings, no environment.
#[derive(Clone, Debug)]
pub struct BuildInput {
    /// The deployment the image is for (usage is charged by it). The builder pushes the image
    /// to the registry repository of its tenant and app, and nowhere else.
    pub identity: WorkloadIdentity,
    /// The runtime of the manifest (`node22`).
    pub runtime: String,
    /// The app archive as uploaded (a zip or tar of the source tree).
    pub archive: Bytes,
    pub limits: Limits,
    /// The build fails with [`RuntimeError::BuildTimedOut`] when it runs longer.
    pub timeout: Duration,
}

impl BuildInput {
    /// The checks every builder makes before it builds.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        self.limits.validate()?;
        if self.runtime.is_empty() {
            return Err(RuntimeError::invalid("runtime", "must not be empty"));
        }
        if self.archive.is_empty() {
            return Err(RuntimeError::invalid("archive", "the archive is empty"));
        }
        if self.timeout.is_zero() {
            return Err(RuntimeError::invalid("timeout", "must be above zero"));
        }
        Ok(())
    }
}

/// One line of a build log. Build logs hold no secrets (the build has none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildLogLine {
    pub at: OffsetDateTime,
    pub text: String,
}

/// The result of a successful build.
#[derive(Clone, Debug)]
pub struct BuildOutput {
    /// Always [`ImageRef::Oci`].
    pub image: ImageRef,
    /// The end of the build log, at most [`MAX_BUILD_LOG_LINES`] lines.
    pub logs: Vec<BuildLogLine>,
    /// How long the build ran (build minutes are charged to the deployer).
    pub took: Duration,
    /// The routes the build had: a subset of [`BUILD_ROUTES`].
    pub routes: Vec<Route>,
    /// The limits the build ran with: exactly [`BuildInput::limits`].
    pub limits: Limits,
}

/// Builds images from archives. The builder reports each build's time to its
/// [`super::RuntimeUsageSink`], failed builds too.
#[async_trait]
pub trait Builder: Send + Sync {
    /// A short name for reports (`memory`, `railpack`).
    fn name(&self) -> &str;

    /// Builds the archive. A build that runs and fails is [`RuntimeError::BuildFailed`] (the
    /// reason holds the end of the build log, bounded); a build that runs longer than
    /// [`BuildInput::timeout`] is ended and is [`RuntimeError::BuildTimedOut`]; an unknown
    /// runtime is [`RuntimeError::UnsupportedRuntime`]. The build time is reported in each of
    /// these cases.
    async fn build(&self, input: BuildInput) -> Result<BuildOutput, RuntimeError>;
}
