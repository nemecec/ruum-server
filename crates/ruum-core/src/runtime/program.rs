//! The conformance program: a tiny step language that the conformance suite passes as the
//! command line of a workload, so the same cases run on every driver.
//!
//! Each argument is one step, run in order. The in-memory driver interprets the steps itself. A
//! real driver's fixture gives an image whose entry point interprets the same steps (for Docker,
//! a small shell script in the test image); see [`Step`] for what each step must do. When the
//! last step is not `serve`, `unhealthy` or `exit`, the program exits with code 0.

use std::time::Duration;

/// One step of the conformance program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// `print:<text>`: writes the text and a line end to standard output.
    Print(String),
    /// `write:<text>`: writes the text to standard output without a line end (one write).
    Write(String),
    /// `stderr:<text>`: writes the text and a line end to standard error.
    Stderr(String),
    /// `stderr-write:<text>`: writes the text to standard error without a line end (one write).
    StderrWrite(String),
    /// `env:<NAME>`: writes `NAME=<value>` and a line end (an empty value when unset). The
    /// variables include the ones the platform sets, such as `RUUM_TOKEN_FILE`.
    Env(String),
    /// `token`: reads the file named by `RUUM_TOKEN_FILE` ([`super::TOKEN_FILE`]) at this step
    /// and writes its content (the current workload token) and a line end.
    Token,
    /// `token-sha`: reads the token file like `token` and writes `token-sha:`, the first 16 hex
    /// characters of the SHA-256 of the token ([`token_sha`]), and a line end. The suite tells
    /// the old token from the new one by it without logging either. A short hash of a
    /// high-entropy token is not a secret.
    TokenSha,
    /// `sleep:<ms>`: waits.
    Sleep(Duration),
    /// `exit:<code>`: exits with the code.
    Exit(i32),
    /// `serve`: answers the health check with 200 and runs until stopped.
    Serve,
    /// `unhealthy`: answers the health check with 500 and runs until stopped.
    Unhealthy,
    /// `alloc:<bytes>`: allocates and touches that much more memory, and keeps it.
    Alloc(u64),
    /// `disk:<bytes>`: writes that much more data to the scratch disk. When the disk is full it
    /// writes `disk:full` and a line end to standard output and exits with code 1.
    Disk(u64),
}

/// The line that [`Step::Disk`] writes when the disk is full.
pub const DISK_FULL: &str = "disk:full";

/// The line that [`Step::TokenSha`] writes for a token.
pub fn token_sha(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    format!("token-sha:{}", &hash[..16])
}

impl Step {
    /// The command line argument of the step.
    pub fn arg(&self) -> String {
        match self {
            Step::Print(t) => format!("print:{t}"),
            Step::Write(t) => format!("write:{t}"),
            Step::Stderr(t) => format!("stderr:{t}"),
            Step::StderrWrite(t) => format!("stderr-write:{t}"),
            Step::Env(n) => format!("env:{n}"),
            Step::Token => "token".to_owned(),
            Step::TokenSha => "token-sha".to_owned(),
            Step::Sleep(d) => format!("sleep:{}", d.as_millis()),
            Step::Exit(c) => format!("exit:{c}"),
            Step::Serve => "serve".to_owned(),
            Step::Unhealthy => "unhealthy".to_owned(),
            Step::Alloc(b) => format!("alloc:{b}"),
            Step::Disk(b) => format!("disk:{b}"),
        }
    }

    /// Reads one argument; `None` when it is not a step.
    pub fn parse(arg: &str) -> Option<Step> {
        let (verb, rest) = match arg.split_once(':') {
            Some((v, r)) => (v, Some(r)),
            None => (arg, None),
        };
        Some(match (verb, rest) {
            ("print", Some(t)) => Step::Print(t.to_owned()),
            ("write", Some(t)) => Step::Write(t.to_owned()),
            ("stderr", Some(t)) => Step::Stderr(t.to_owned()),
            ("stderr-write", Some(t)) => Step::StderrWrite(t.to_owned()),
            ("env", Some(n)) if !n.is_empty() => Step::Env(n.to_owned()),
            ("token", None) => Step::Token,
            ("token-sha", None) => Step::TokenSha,
            ("sleep", Some(ms)) => Step::Sleep(Duration::from_millis(ms.parse().ok()?)),
            ("exit", Some(c)) => Step::Exit(c.parse().ok()?),
            ("serve", None) => Step::Serve,
            ("unhealthy", None) => Step::Unhealthy,
            ("alloc", Some(b)) => Step::Alloc(b.parse().ok()?),
            ("disk", Some(b)) => Step::Disk(b.parse().ok()?),
            _ => return None,
        })
    }
}

/// The command line of a program.
pub fn command(steps: &[Step]) -> Vec<String> {
    steps.iter().map(Step::arg).collect()
}
