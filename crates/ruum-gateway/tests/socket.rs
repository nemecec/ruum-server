//! The socket check of the gateway binary (decision 0006 G point 1, a requirement for the shared
//! containers on app networks; card G-1a): `ruum-gateway` listens only on its configured address
//! and port and opens no other listener (TCP or UDP). The cross-network forwarding test of the
//! sandbox waits for cards G-1b and I-4.
//!
//! Linux only: it reads the process's sockets from `/proc`. The build server and CI run Linux.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// The socket inodes that the process `pid` holds.
fn socket_inodes(pid: u32) -> HashSet<String> {
    let mut out = HashSet::new();
    for e in std::fs::read_dir(format!("/proc/{pid}/fd")).expect("read the fd list") {
        let Ok(e) = e else { continue };
        if let Ok(target) = std::fs::read_link(e.path()) {
            let t = target.to_string_lossy().to_string();
            if let Some(inode) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']')) {
                out.insert(inode.to_string());
            }
        }
    }
    out
}

/// `(local address, state)` of the sockets of `table` (`/proc/net/tcp` and friends) whose inode
/// is in `inodes`.
fn sockets(table: &str, inodes: &HashSet<String>) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(format!("/proc/net/{table}")) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() > 9 && inodes.contains(f[9])).then(|| (f[1].to_string(), f[3].to_string()))
        })
        .collect()
}

#[test]
fn the_binary_listens_only_on_its_configured_address() {
    let dir = std::env::temp_dir().join(format!("ruum-gw-socket-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let key = dir.join("user-token.key");
    {
        use base64::Engine as _;
        let mut seed = [0u8; 32];
        rand::fill(&mut seed[..]);
        std::fs::write(
            &key,
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed),
        )
        .unwrap();
    }
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_ruum-gateway"))
        .env_clear()
        .env("RUUM_GATEWAY_BIND", format!("127.0.0.1:{port}"))
        .env("RUUM_GATEWAY_ISSUER", "https://ruum.localhost")
        .env("RUUM_GATEWAY_INTERNAL_URL", "http://127.0.0.1:9")
        .env(
            "RUUM_GATEWAY_DATABASE_URL",
            "postgres://ruum_gateway@127.0.0.1:9/ruum",
        )
        .env("RUUM_GATEWAY_TOKEN_KEY_FILE", &key)
        .env("RUST_LOG", "error")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start ruum-gateway");
    let pid = child.id();
    let start = Instant::now();
    let mut conn = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(c) => break c,
            Err(_) if start.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                let _ = child.kill();
                panic!("the gateway did not listen on its address: {e}");
            }
        }
    };
    // It answers with its own 404 for a host outside the apps domain.
    conn.write_all(b"GET / HTTP/1.1\r\nHost: garden.acme.example.com\r\nConnection: close\r\n\r\n")
        .unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut answer = Vec::new();
    let _ = conn.read_to_end(&mut answer);
    let head = String::from_utf8_lossy(&answer);
    let inodes = socket_inodes(pid);
    let listening: Vec<String> = ["tcp", "tcp6"]
        .iter()
        .flat_map(|t| sockets(t, &inodes))
        .filter(|(_, state)| state == "0A")
        .map(|(local, _)| local)
        .collect();
    let udp: Vec<(String, String)> = ["udp", "udp6"]
        .iter()
        .flat_map(|t| sockets(t, &inodes))
        .collect();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        head.starts_with("HTTP/1.1 404"),
        "expected the gateway's 404, got {:?}",
        head.lines().next()
    );
    let want = format!("0100007F:{port:04X}");
    assert_eq!(
        listening,
        vec![want],
        "the gateway must listen on 127.0.0.1:{port} only"
    );
    assert!(udp.is_empty(), "the gateway opened UDP sockets: {udp:?}");
}
