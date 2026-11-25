//! A small HTTP/1.1 client for the Kubernetes launch channel: one `POST` with a bearer token to
//! `http://<address>:<port>`, the answer read under a byte cap. No TLS, no redirects, no proxy
//! (the sandbox's proxy variables never apply: the supervisor reads none of them).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

/// What a request can end with.
#[derive(Debug)]
pub enum HttpError {
    /// Connecting, writing or reading failed, or the answer did not parse: worth another try.
    Transport(String),
    /// The server answered with this status (not 200).
    Status(u16),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpError::Transport(e) => write!(f, "transport: {e}"),
            HttpError::Status(s) => write!(f, "status {s}"),
        }
    }
}

/// The address of `http://<ip>:<port>` (an IP address: the sandbox has no DNS). `None` for any
/// other form (a path, a user, a name, `https`).
pub fn parse_base(url: &str) -> Option<SocketAddr> {
    let rest = url.strip_prefix("http://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') || rest.contains('@') {
        return None;
    }
    rest.parse::<SocketAddr>().ok()
}

/// `POST <path>` with `Authorization: Bearer <token>` and no body. The answer body of a `200`,
/// at most `max` bytes.
pub fn post(
    addr: SocketAddr,
    path: &str,
    bearer: &str,
    max: usize,
    timeout: Duration,
) -> Result<Vec<u8>, HttpError> {
    let t = |e: std::io::Error| HttpError::Transport(e.kind().to_string());
    if bearer.contains(['\r', '\n']) || path.contains(['\r', '\n', ' ']) {
        return Err(HttpError::Transport("a bad header value".to_owned()));
    }
    let mut s = TcpStream::connect_timeout(&addr, timeout).map_err(t)?;
    s.set_read_timeout(Some(timeout)).map_err(t)?;
    s.set_write_timeout(Some(timeout)).map_err(t)?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {bearer}\r\nContent-Length: 0\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes()).map_err(t)?;
    // Head and body together, at most the cap plus room for the head.
    let mut raw = Vec::new();
    let limit = max + 16 * 1024;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = s.read(&mut buf).map_err(t)?;
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > limit {
            return Err(HttpError::Transport("the answer is too large".to_owned()));
        }
    }
    parse_answer(&raw, max)
}

fn bad(what: &str) -> HttpError {
    HttpError::Transport(what.to_owned())
}

/// The body of a complete HTTP/1.1 answer (`Content-Length`, chunked, or up to the end).
pub fn parse_answer(raw: &[u8], max: usize) -> Result<Vec<u8>, HttpError> {
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no end of the head"))?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| bad("the head is not text"))?;
    let body = &raw[end + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or_else(|| bad("no status line"))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(bad("not HTTP/1"));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("no status"))?;
    let mut length: Option<usize> = None;
    let mut chunked = false;
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            return Err(bad("a header without a colon"));
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "content-length" {
            length = Some(v.parse().map_err(|_| bad("a bad Content-Length"))?);
        } else if k == "transfer-encoding" {
            chunked = v.eq_ignore_ascii_case("chunked");
            if !chunked {
                return Err(bad("an unknown transfer encoding"));
            }
        }
    }
    if status != 200 {
        return Err(HttpError::Status(status));
    }
    let out = if chunked {
        dechunk(body, max)?
    } else if let Some(n) = length {
        if n > max || body.len() < n {
            return Err(bad("the body is cut or too large"));
        }
        body[..n].to_vec()
    } else {
        if body.len() > max {
            return Err(bad("the body is too large"));
        }
        body.to_vec()
    };
    Ok(out)
}

fn dechunk(mut body: &[u8], max: usize) -> Result<Vec<u8>, HttpError> {
    let mut out = Vec::new();
    loop {
        let eol = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or_else(|| bad("a cut chunk"))?;
        let size_text = std::str::from_utf8(&body[..eol]).map_err(|_| bad("a bad chunk size"))?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16).map_err(|_| bad("a bad chunk size"))?;
        body = &body[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 || &body[size..size + 2] != b"\r\n" {
            return Err(bad("a cut chunk"));
        }
        out.extend_from_slice(&body[..size]);
        if out.len() > max {
            return Err(bad("the body is too large"));
        }
        body = &body[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_plain_ip_base_parses() {
        assert!(parse_base("http://10.0.0.1:8091").is_some());
        assert!(parse_base("http://10.0.0.1:8091/").is_some());
        assert!(parse_base("http://[fd00::1]:8091").is_some());
        for bad in [
            "https://10.0.0.1:8091",
            "http://server:8091",
            "http://10.0.0.1:8091/v1",
            "http://u@10.0.0.1:8091",
            "http://10.0.0.1",
        ] {
            assert!(parse_base(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn answers_parse_with_length_chunks_or_the_end() {
        let a = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello";
        assert_eq!(parse_answer(a, 100).unwrap(), b"hello");
        let c = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2\r\nlo\r\n0\r\n\r\n";
        assert_eq!(parse_answer(c, 100).unwrap(), b"hello");
        let e = b"HTTP/1.0 200 OK\r\n\r\nhello";
        assert_eq!(parse_answer(e, 100).unwrap(), b"hello");
        let f = b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n";
        assert!(matches!(parse_answer(f, 100), Err(HttpError::Status(403))));
        let big = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello";
        assert!(parse_answer(big, 4).is_err());
        let cut = b"HTTP/1.1 200 OK\r\ncontent-length: 9\r\n\r\nhello";
        assert!(parse_answer(cut, 100).is_err());
        let cut_chunk = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nhel";
        assert!(parse_answer(cut_chunk, 100).is_err());
    }

    #[test]
    fn a_post_reaches_a_local_server() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
            }
            s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
                .unwrap();
            String::from_utf8(req).unwrap()
        });
        let body = post(addr, "/v1/launch", "tok", 100, Duration::from_secs(5)).unwrap();
        assert_eq!(body, b"{}");
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /v1/launch HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Authorization: Bearer tok\r\n"), "{req}");
        assert!(post(addr, "/x", "a\r\nb", 100, Duration::from_secs(1)).is_err());
    }
}
