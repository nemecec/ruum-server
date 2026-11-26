//! The launch data of one run and the token answer: what a driver gives the supervisor.
//!
//! - Docker: the driver writes [`Launch`] as JSON to the container's standard input and closes
//!   it (`OpenStdin`, `StdinOnce`); the supervisor reads it before the app starts.
//! - Kubernetes: the server answers [`Launch`] at `POST /v1/launch` and [`TokenAnswer`] at
//!   `POST /v1/token` of its supervisor listener, to a pod that presents its projected service
//!   account token ([`IDENTITY_FILE`]).
//!
//! Neither type has a `Debug` that shows a value.

use crate::redact::{
    MAX_KEPT_TOKENS, MAX_SECRET_TOTAL_BYTES, MAX_SECRET_VALUE_BYTES, MAX_SECRET_VALUES,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The version of [`Launch`] and [`TokenAnswer`]. A supervisor refuses another one.
pub const LAUNCH_VERSION: u32 = 1;

/// Where the supervisor binary is inside the sandbox (both drivers).
pub const SUPERVISOR_PATH: &str = "/ruum/bin/ruum-supervisor";
/// The folder of [`SUPERVISOR_PATH`] (the read-only `ruum-bin` volume on Kubernetes).
pub const SUPERVISOR_DIR: &str = "/ruum/bin";
/// The projected service account token of a Kubernetes sandbox (audience `ruum-supervisor`).
pub const IDENTITY_FILE: &str = "/ruum/id/token";
/// The folder of [`IDENTITY_FILE`] (the projected volume).
pub const IDENTITY_DIR: &str = "/ruum/id";
/// The file the kubelet reads the termination message from. The supervisor empties it when the
/// app ends (the app could have written a value there).
pub const TERMINATION_LOG: &str = "/dev/termination-log";
/// The exit code of a run whose launch failed (no data, a refused credential, data over the
/// limits). The supervisor prints one line that names no value.
pub const EXIT_LAUNCH_FAILED: i32 = 125;
/// The exit code when the app's command did not start (not found, not executable).
pub const EXIT_NOT_STARTED: i32 = 127;
/// The longest line the supervisor lets out (the drivers' cap): a longer one is cut by the
/// redactor before its check.
pub const MAX_LINE_BYTES: usize = 16 * 1024;
/// The most entries of [`Launch::redact`]: the secret limit plus the kept tokens, the same
/// allowance as the check of all values together. A spec has at most `MAX_SECRET_VALUES` values
/// (its current token among them); after rotations the driver adds up to `MAX_KEPT_TOKENS - 1`
/// earlier tokens, which stay valid until they expire, so a lower cap would leave one out (Kimi
/// re-check of I6-20 in run 16, defect 3).
pub const MAX_REDACT: usize = MAX_SECRET_VALUES + MAX_KEPT_TOKENS;
/// The largest launch answer the supervisor reads (values, tokens and the command, as JSON).
pub const MAX_LAUNCH_BYTES: usize = 2 * 1024 * 1024;
/// The most arguments of the app's command, and their bytes in all.
pub const MAX_ARGS: usize = 4096;
pub const MAX_ARG_BYTES: usize = 256 * 1024;

/// The launch data of one run.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub v: u32,
    /// The app's command: the image's entrypoint, then the spec's command or the image's `Cmd`.
    pub argv: Vec<String>,
    /// The secret environment values (name, value). The plain ones are in the container's own
    /// environment.
    pub env: Vec<(String, String)>,
    /// More values to redact that the app does not get: the previous secret values and the
    /// earlier workload tokens.
    #[serde(default)]
    pub redact: Vec<String>,
    /// The current workload token. Kubernetes: the supervisor writes it to the token file and
    /// redacts it. Docker: `None` (the driver writes the token file; the supervisor reads it).
    #[serde(default)]
    pub token: Option<String>,
    /// `RLIMIT_NPROC` for the app (Kubernetes, where gVisor counts per sandbox). `None`: no limit
    /// from the supervisor (the Docker driver keeps the engine's `pids-limit`).
    #[serde(default)]
    pub pids_limit: Option<u64>,
    /// How often the supervisor asks for the current token (Kubernetes), in seconds.
    #[serde(default)]
    pub token_poll_secs: Option<u64>,
}

impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("v", &self.v)
            .field("args", &self.argv.len())
            .field(
                "env",
                &self.env.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            )
            .field("redact", &self.redact.len())
            .field("token", &self.token.as_ref().map(|_| "<hidden>"))
            .field("pids_limit", &self.pids_limit)
            .finish()
    }
}

/// The answer of `POST /v1/token`: the current workload token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAnswer {
    pub v: u32,
    pub token: String,
}

impl fmt::Debug for TokenAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenAnswer")
            .field("v", &self.v)
            .finish_non_exhaustive()
    }
}

/// Whether `name` is an environment name the spec allows (`[A-Za-z_][A-Za-z0-9_]*`).
pub fn env_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Checks one value to redact against the redactor's limit.
fn value_ok(v: &str) -> Result<(), String> {
    if v.len() > MAX_SECRET_VALUE_BYTES {
        return Err(format!(
            "a value is longer than {MAX_SECRET_VALUE_BYTES} bytes"
        ));
    }
    if v.contains('\0') {
        return Err("a value holds a NUL byte".to_owned());
    }
    Ok(())
}

impl Launch {
    /// Every value the supervisor redacts: the secret values, the other values and the token.
    /// Empty values are left out.
    pub fn values(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .env
            .iter()
            .map(|(_, v)| v.clone())
            .chain(self.redact.iter().cloned())
            .chain(self.token.iter().cloned())
            .filter(|v| !v.is_empty())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The checks before anything starts: the version, a command, environment names, and the
    /// redactor's limits (`ruum-core` `runtime::redact`: at most 256 values, 16 KiB each and
    /// 256 KiB in all, plus at most 16 tokens; [`MAX_REDACT`] entries to redact). `Err` names
    /// the rule, never a value.
    pub fn check(&self) -> Result<(), String> {
        if self.v != LAUNCH_VERSION {
            return Err(format!(
                "launch data version {} is not {LAUNCH_VERSION}",
                self.v
            ));
        }
        if self.argv.is_empty() || self.argv[0].is_empty() {
            return Err("the launch data has no command".to_owned());
        }
        if self.argv.len() > MAX_ARGS
            || self.argv.iter().map(String::len).sum::<usize>() > MAX_ARG_BYTES
        {
            return Err("the command is too long".to_owned());
        }
        if self.argv.iter().any(|a| a.contains('\0')) {
            return Err("an argument holds a NUL byte".to_owned());
        }
        let mut names = std::collections::HashSet::new();
        for (name, value) in &self.env {
            if !env_name_ok(name) {
                return Err("an environment name is not [A-Za-z_][A-Za-z0-9_]*".to_owned());
            }
            if !names.insert(name.as_str()) {
                return Err("an environment name is given twice".to_owned());
            }
            value_ok(value)?;
        }
        for v in self.redact.iter().chain(self.token.iter()) {
            value_ok(v)?;
        }
        if self.redact.len() > MAX_REDACT {
            return Err("too many values to redact".to_owned());
        }
        let values = self.values();
        if values.len() > MAX_SECRET_VALUES + MAX_KEPT_TOKENS {
            return Err(format!(
                "at most {MAX_SECRET_VALUES} values and {MAX_KEPT_TOKENS} tokens"
            ));
        }
        let total: usize = values.iter().map(String::len).sum();
        if total > MAX_SECRET_TOTAL_BYTES + MAX_KEPT_TOKENS * MAX_SECRET_VALUE_BYTES {
            return Err("the values are too large together".to_owned());
        }
        if self.pids_limit.is_some_and(|p| p < 8) {
            return Err("a PID limit below 8 cannot run the supervisor".to_owned());
        }
        Ok(())
    }
}

impl TokenAnswer {
    pub fn check(&self) -> Result<(), String> {
        if self.v != LAUNCH_VERSION {
            return Err(format!(
                "token answer version {} is not {LAUNCH_VERSION}",
                self.v
            ));
        }
        if self.token.is_empty() {
            return Err("the token answer is empty".to_owned());
        }
        value_ok(&self.token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            v: LAUNCH_VERSION,
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            env: vec![("API_KEY".into(), "s3cr3t".into())],
            redact: vec!["old".into()],
            token: Some("tok".into()),
            pids_limit: None,
            token_poll_secs: None,
        }
    }

    #[test]
    fn debug_shows_no_value() {
        let l = launch();
        let d = format!("{l:?}");
        assert!(
            !d.contains("s3cr3t") && !d.contains("tok\"") && !d.contains("old"),
            "{d}"
        );
        let t = TokenAnswer {
            v: 1,
            token: "s3cr3t-token".into(),
        };
        assert!(!format!("{t:?}").contains("s3cr3t"));
    }

    #[test]
    fn the_checks_refuse_bad_launch_data() {
        assert!(launch().check().is_ok());
        let mut l = launch();
        l.v = 2;
        assert!(l.check().is_err());
        let mut l = launch();
        l.argv.clear();
        assert!(l.check().is_err());
        let mut l = launch();
        l.env.push(("1BAD".into(), "x".into()));
        assert!(l.check().is_err());
        let mut l = launch();
        l.env.push(("API_KEY".into(), "y".into()));
        assert!(l.check().is_err(), "a name twice");
        let mut l = launch();
        l.env[0].1 = "x".repeat(MAX_SECRET_VALUE_BYTES + 1);
        assert!(l.check().is_err());
        let mut l = launch();
        l.redact = (0..=MAX_REDACT).map(|i| format!("v{i}")).collect();
        assert!(l.check().is_err(), "more entries to redact than the cap");
        let mut l = launch();
        l.redact = (0..MAX_REDACT).map(|i| format!("v{i}")).collect();
        assert!(
            l.check().is_err(),
            "more values together than the secret limit and the tokens"
        );
        let mut l = launch();
        l.pids_limit = Some(3);
        assert!(l.check().is_err());
        let mut l = launch();
        l.argv.push("a\0b".into());
        assert!(l.check().is_err());
    }

    /// The largest valid spec (255 previous values and its token) after 15 rotations: every
    /// earlier token fits (Kimi re-check of I6-20 in run 16, defect 3).
    #[test]
    fn the_largest_spec_with_every_kept_token_passes() {
        let l = Launch {
            v: LAUNCH_VERSION,
            argv: vec!["/app".into()],
            env: Vec::new(),
            redact: (0..MAX_SECRET_VALUES - 1)
                .map(|i| format!("previous-{i}"))
                .chain((0..MAX_KEPT_TOKENS - 1).map(|i| format!("earlier-token-{i}")))
                .collect(),
            token: Some("current-token".into()),
            pids_limit: None,
            token_poll_secs: None,
        };
        assert_eq!(l.check(), Ok(()));
    }

    #[test]
    fn values_cover_env_redact_and_token() {
        let v = launch().values();
        assert_eq!(v, vec!["old", "s3cr3t", "tok"]);
    }
}
