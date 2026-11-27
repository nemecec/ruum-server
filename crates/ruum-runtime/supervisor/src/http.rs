//! A small HTTP/1.1 client for the Kubernetes launch channel: one `POST` with a bearer token to
//! `http://<address>:<port>`, or `https://<address>:<port>` with TLS 1.3 and a pinned
//! certificate (backlog I620-2), the answer read under a byte cap. No redirects, no proxy (the
//! sandbox's proxy variables never apply: the supervisor reads none of them).
//!
//! TLS: the supervisor trusts exactly one certificate, the one whose SHA-256 is the pin from its
//! command line (`--server-pin sha256:<hex>`; the driver derives the listener's key from the
//! record key and puts the pin into the pod, `crates/ruum-runtime` `src/kube/tls.rs`). No
//! certificate authority, no name and no expiry are checked; the handshake's signature is checked
//! with the pinned certificate's key, so only a holder of the listener's key can answer.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
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

/// The address of `http://<ip>:<port>` or `https://<ip>:<port>` (an IP address: the sandbox
/// has no DNS), and whether it is `https`. `None` for any other form (a path, a user, a name).
pub fn parse_base(url: &str) -> Option<(SocketAddr, bool)> {
    let (rest, tls) = match url.strip_prefix("https://") {
        Some(r) => (r, true),
        None => (url.strip_prefix("http://")?, false),
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') || rest.contains('@') {
        return None;
    }
    rest.parse::<SocketAddr>().ok().map(|a| (a, tls))
}

/// The SHA-256 of `sha256:<64 hex>`; `None` for any other form.
pub fn parse_pin(pin: &str) -> Option<[u8; 32]> {
    let hex = pin.strip_prefix("sha256:")?;
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The verifier that trusts exactly the certificate of the pin.
#[derive(Debug)]
struct Pinned {
    pin: [u8; 32],
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if intermediates.is_empty() && digest == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        // TLS 1.3 only (the client config offers no other version).
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// The TLS 1.3 client configuration that trusts only the certificate of `pin`.
pub fn tls_config(pin: [u8; 32]) -> Result<Arc<rustls::ClientConfig>, HttpError> {
    let provider: Arc<CryptoProvider> = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let algs = provider.signature_verification_algorithms;
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| bad("no TLS 1.3"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, algs }))
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

/// `POST <path>` with `Authorization: Bearer <token>` and no body, over TLS when `pin` is set
/// (the listener's certificate must be the pinned one). The answer body of a `200`, at most
/// `max` bytes.
pub fn post(
    addr: SocketAddr,
    pin: Option<[u8; 32]>,
    path: &str,
    bearer: &str,
    max: usize,
    timeout: Duration,
) -> Result<Vec<u8>, HttpError> {
    let t = |e: std::io::Error| HttpError::Transport(e.kind().to_string());
    if bearer.contains(['\r', '\n']) || path.contains(['\r', '\n', ' ']) {
        return Err(HttpError::Transport("a bad header value".to_owned()));
    }
    let s = TcpStream::connect_timeout(&addr, timeout).map_err(t)?;
    s.set_read_timeout(Some(timeout)).map_err(t)?;
    s.set_write_timeout(Some(timeout)).map_err(t)?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {bearer}\r\nContent-Length: 0\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    match pin {
        None => exchange(s, req.as_bytes(), max, false),
        Some(pin) => {
            let name = ServerName::IpAddress(addr.ip().into());
            let conn = rustls::ClientConnection::new(tls_config(pin)?, name)
                .map_err(|_| bad("the TLS client did not start"))?;
            exchange(rustls::StreamOwned::new(conn, s), req.as_bytes(), max, true)
        }
    }
}

/// Writes `req` and reads the answer to its end. Over TLS (`framed`) the body must carry its
/// length or chunks, so a connection cut by the network cannot shorten it unnoticed.
fn exchange(
    mut s: impl Read + Write,
    req: &[u8],
    max: usize,
    framed: bool,
) -> Result<Vec<u8>, HttpError> {
    let t = |e: std::io::Error| HttpError::Transport(e.kind().to_string());
    s.write_all(req).map_err(t)?;
    s.flush().map_err(t)?;
    // Head and body together, at most the cap plus room for the head.
    let mut raw = Vec::new();
    let limit = max + 16 * 1024;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = match s.read(&mut buf) {
            Ok(n) => n,
            // A TLS peer that closes without close_notify: the framing below decides.
            Err(e) if framed && e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Err(e) => return Err(t(e)),
        };
        if n == 0 {
            break;
        }
        raw.extend_from_slice(&buf[..n]);
        if raw.len() > limit {
            return Err(HttpError::Transport("the answer is too large".to_owned()));
        }
    }
    parse_framed(&raw, max, framed)
}

fn bad(what: &str) -> HttpError {
    HttpError::Transport(what.to_owned())
}

/// The body of a complete HTTP/1.1 answer (`Content-Length`, chunked, or up to the end).
pub fn parse_answer(raw: &[u8], max: usize) -> Result<Vec<u8>, HttpError> {
    parse_framed(raw, max, false)
}

/// [`parse_answer`]; with `framed`, an answer without `Content-Length` or chunks is refused.
fn parse_framed(raw: &[u8], max: usize, framed: bool) -> Result<Vec<u8>, HttpError> {
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
        if framed {
            return Err(bad("an answer without a length"));
        }
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
        assert!(!parse_base("http://10.0.0.1:8091").unwrap().1);
        assert!(parse_base("http://10.0.0.1:8091/").is_some());
        assert!(parse_base("http://[fd00::1]:8091").is_some());
        assert!(parse_base("https://10.0.0.1:8091").unwrap().1);
        for bad in [
            "ftp://10.0.0.1:8091",
            "https://server:8091",
            "http://server:8091",
            "http://10.0.0.1:8091/v1",
            "http://u@10.0.0.1:8091",
            "http://10.0.0.1",
        ] {
            assert!(parse_base(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn only_a_sha256_pin_parses() {
        let hex = "ab".repeat(32);
        assert_eq!(parse_pin(&format!("sha256:{hex}")), Some([0xab; 32]));
        for bad in [
            hex.clone(),
            format!("sha256:{}", &hex[..62]),
            format!("sha256:{hex}00"),
            format!("sha1:{hex}"),
            format!("sha256:{}", "zz".repeat(32)),
        ] {
            assert!(parse_pin(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn a_framed_answer_needs_a_length() {
        let e = b"HTTP/1.0 200 OK\r\n\r\nhello";
        assert!(parse_framed(e, 100, true).is_err());
        let a = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello";
        assert_eq!(parse_framed(a, 100, true).unwrap(), b"hello");
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
        let body = post(addr, None, "/v1/launch", "tok", 100, Duration::from_secs(5)).unwrap();
        assert_eq!(body, b"{}");
        let req = server.join().unwrap();
        assert!(req.starts_with("POST /v1/launch HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Authorization: Bearer tok\r\n"), "{req}");
        assert!(post(addr, None, "/x", "a\r\nb", 100, Duration::from_secs(1)).is_err());
    }
}
