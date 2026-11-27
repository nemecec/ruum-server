//! `ruum-supervisor`: the first process of an app sandbox (see the library docs).
//!
//! ```text
//! ruum-supervisor install <dest>
//! ruum-supervisor run --launch-stdin --token-file <path>
//! ruum-supervisor run --launch-url http://<ip>:<port> --identity-file <path> --token-file <path>
//! ruum-supervisor run --launch-url https://<ip>:<port> --server-pin sha256:<hex> --identity-file <path> --token-file <path>
//! ruum-supervisor exec -- <app argv...>
//! ```
//!
//! `exec` is the supervisor's own step between the fork and the app: it empties the signal mask
//! (the supervisor blocks the forwarded signals, and a child inherits the mask through the fork
//! and `execve`), then replaces itself with the app.
//!
//! Arguments after these are ignored. Every message names a step or a rule, never a value.

use ruum_supervisor::protocol::{EXIT_LAUNCH_FAILED, EXIT_NOT_STARTED, TERMINATION_LOG};
use ruum_supervisor::run::{self, Options, Source};
use std::io::Write;
use std::path::PathBuf;

fn fail(msg: &str) -> ! {
    let _ = writeln!(std::io::stderr(), "ruum-supervisor: {msg}");
    std::process::exit(EXIT_LAUNCH_FAILED);
}

fn main() {
    // Non-dumpable first: the app (the same user) cannot open this process's /proc files (its
    // output streams, its memory) or trace it (docs/STEP3.md "The sandbox supervisor" 2.1).
    if nix::sys::prctl::set_dumpable(false).is_err() {
        fail("cannot make the supervisor non-dumpable");
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("exec") => {
            let argv = match args.get(1).map(String::as_str) {
                Some("--") => &args[2..],
                _ => fail("exec needs -- and the app's command"),
            };
            if argv.is_empty() {
                fail("exec needs the app's command");
            }
            let e = run::exec_app(argv);
            let _ = writeln!(
                std::io::stderr(),
                "ruum-supervisor: the app's command did not start: {}",
                e.kind()
            );
            std::process::exit(EXIT_NOT_STARTED);
        }
        Some("install") => {
            let Some(dest) = args.get(1) else {
                fail("install needs a destination");
            };
            if let Err(e) = run::install(&PathBuf::from(dest)) {
                fail(&e);
            }
        }
        Some("run") => {
            let mut launch_stdin = false;
            let mut launch_url = None;
            let mut identity = None;
            let mut token_file = None;
            let mut server_pin = None;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--launch-stdin" {
                    launch_stdin = true;
                    continue;
                }
                let slot = match a.as_str() {
                    "--launch-url" => &mut launch_url,
                    "--identity-file" => &mut identity,
                    "--token-file" => &mut token_file,
                    "--server-pin" => &mut server_pin,
                    _ => break,
                };
                *slot = it.next().cloned();
            }
            let Some(token_file) = token_file else {
                fail("run needs --token-file");
            };
            let source = match (launch_stdin, launch_url, identity) {
                (true, None, None) => Source::Stdin,
                (false, Some(u), Some(i)) => {
                    let Some((base, tls)) = ruum_supervisor::http::parse_base(&u) else {
                        fail("--launch-url must be http://<ip>:<port> or https://<ip>:<port>");
                    };
                    // TLS needs the pin, and a pin is never ignored (backlog I620-2).
                    let pin = match (tls, server_pin.as_deref()) {
                        (true, Some(p)) => match ruum_supervisor::http::parse_pin(p) {
                            Some(pin) => Some(pin),
                            None => fail("--server-pin must be sha256:<64 hex>"),
                        },
                        (false, None) => None,
                        (true, None) => fail("an https launch URL needs --server-pin"),
                        (false, Some(_)) => fail("--server-pin needs an https launch URL"),
                    };
                    Source::Http {
                        base,
                        identity: PathBuf::from(i),
                        pin,
                    }
                }
                _ => fail("run needs --launch-stdin, or --launch-url with --identity-file"),
            };
            let mut opts = Options::new(source, PathBuf::from(token_file));
            opts.init = true;
            opts.termination_log = Some(PathBuf::from(TERMINATION_LOG));
            let launch = match run::load(&opts) {
                Ok(l) => l,
                Err(e) => fail(&format!("the launch failed: {e}")),
            };
            let code = run::supervise(
                &opts,
                launch,
                Box::new(std::io::stdout()),
                Box::new(std::io::stderr()),
            );
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(code);
        }
        _ => fail("usage: ruum-supervisor install <dest> | run ..."),
    }
}
