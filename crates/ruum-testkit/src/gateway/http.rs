//! A raw HTTP/1.1 client and a cookie jar with browser rules.
//!
//! The client writes the request target byte for byte (no URL parser normalizes `..`, `%2e%2e`
//! or `\` on the way), sends any `Host` header to the listener it is given (so every app host
//! name "resolves" to the gateway), and keeps repeated headers (several `Cookie` headers) in
//! order. Each request uses its own connection (`Connection: close`).

use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A request to send.
#[derive(Clone, Debug)]
pub struct Req {
    pub method: String,
    /// The `Host` header (no port).
    pub host: String,
    /// The request target, sent as it is (`/a/../b`, `/a%2fb`).
    pub target: String,
    /// Headers in order; a name may repeat.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Req {
    pub fn new(method: &str, host: &str, target: &str) -> Self {
        Req {
            method: method.to_string(),
            host: host.to_string(),
            target: target.to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn get(host: &str, target: &str) -> Self {
        Self::new("GET", host, target)
    }

    pub fn post(host: &str, target: &str) -> Self {
        Self::new("POST", host, target)
    }

    /// Adds a header (also when one of that name is there).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// A JSON body with its content type.
    pub fn json(self, v: &serde_json::Value) -> Self {
        self.header("Content-Type", "application/json")
            .body(v.to_string())
    }

    /// The path of the target (before `?`).
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("/")
    }

    /// `METHOD host target`, for messages (no header values).
    pub fn label(&self) -> String {
        format!(
            "{} {}{}",
            self.method,
            self.host,
            self.target.escape_debug()
        )
    }
}

/// An answer.
#[derive(Clone, Debug)]
pub struct Resp {
    pub status: u16,
    /// Headers in order, names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Resp {
    /// The first value of header `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Every value of header `name`.
    pub fn headers_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }

    /// The raw `Set-Cookie` headers.
    pub fn set_cookies(&self) -> Vec<&str> {
        self.headers_all("set-cookie")
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// The first 200 characters of the body, for messages.
    pub fn short_body(&self) -> String {
        let t = self.text();
        let t: String = t.chars().take(200).collect();
        format!("{:?}", t)
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308)
    }
}

/// Sends `req` to `addr` and reads the whole answer. A connection error, a malformed answer, a
/// body shorter than its `Content-Length` or a timeout is an error.
pub async fn send(addr: SocketAddr, req: &Req, timeout: Duration) -> Result<Resp, String> {
    tokio::time::timeout(timeout, send_inner(addr, req))
        .await
        .map_err(|_| format!("timeout after {} s", timeout.as_secs()))?
}

async fn send_inner(addr: SocketAddr, req: &Req) -> Result<Resp, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("connect {addr}: {e}"))?;
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\n",
        req.method, req.target, req.host
    );
    let mut has_connection = false;
    let mut has_length = false;
    for (n, v) in &req.headers {
        if n.eq_ignore_ascii_case("connection") {
            has_connection = true;
            let v = if v.to_ascii_lowercase().contains("close") {
                v.clone()
            } else {
                format!("close, {v}")
            };
            head.push_str(&format!("{n}: {v}\r\n"));
            continue;
        }
        if n.eq_ignore_ascii_case("content-length") {
            has_length = true;
        }
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    if !has_connection {
        head.push_str("Connection: close\r\n");
    }
    if !has_length
        && (!req.body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH"))
    {
        head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(&req.body);
    stream
        .write_all(&out)
        .await
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    // A server may close the connection before it read the whole request (a refusal); what it
    // wrote still counts.
    match stream.read_to_end(&mut buf).await {
        Ok(_) => {}
        Err(_) if !buf.is_empty() => {}
        Err(e) => return Err(format!("read: {e}")),
    }
    parse_response(&buf, req.method == "HEAD")
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parses an HTTP/1.1 answer (one; informational `1xx` answers before it are skipped).
pub fn parse_response(mut buf: &[u8], head_only: bool) -> Result<Resp, String> {
    loop {
        let end = find(buf, b"\r\n\r\n").ok_or_else(|| {
            if buf.is_empty() {
                "the connection closed without an answer".to_string()
            } else {
                "an answer without the end of its head".to_string()
            }
        })?;
        let head = std::str::from_utf8(&buf[..end]).map_err(|_| "a head that is not UTF-8")?;
        let rest = &buf[end + 4..];
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        if !version.starts_with("HTTP/1.") {
            return Err(format!("not an HTTP/1 answer: {status_line:?}"));
        }
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("bad status line {status_line:?}"))?;
        let mut headers = Vec::new();
        for l in lines {
            if let Some((n, v)) = l.split_once(':') {
                headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        if (100..200).contains(&status) {
            buf = rest;
            continue;
        }
        let get = |name: &str| {
            headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        let body = if head_only || status == 204 || status == 304 {
            Vec::new()
        } else if get("transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false)
        {
            dechunk(rest)?
        } else if let Some(len) = get("content-length") {
            let len: usize = len
                .parse()
                .map_err(|_| format!("bad Content-Length {len:?}"))?;
            if rest.len() < len {
                return Err(format!(
                    "the body has {} bytes, Content-Length says {len}",
                    rest.len()
                ));
            }
            rest[..len].to_vec()
        } else {
            rest.to_vec()
        };
        return Ok(Resp {
            status,
            headers,
            body,
        });
    }
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = find(b, b"\r\n").ok_or("a chunk without its size line")?;
        let size_text = std::str::from_utf8(&b[..eol]).map_err(|_| "a bad chunk size")?;
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("a bad chunk size {size_text:?}"))?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("a chunk shorter than its size".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

// ---------------------------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------------------------

/// A parsed `Set-Cookie` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetCookie {
    pub name: String,
    pub value: String,
    /// Attributes, names in lower case.
    pub attrs: Vec<(String, Option<String>)>,
}

impl SetCookie {
    /// Parses `name=value; Attr; Attr=v`. `None` when there is no `=` in the first pair.
    pub fn parse(raw: &str) -> Option<SetCookie> {
        let mut parts = raw.split(';');
        let first = parts.next()?;
        let (name, value) = first.split_once('=')?;
        let attrs = parts
            .map(|a| {
                let a = a.trim();
                match a.split_once('=') {
                    Some((k, v)) => (k.trim().to_ascii_lowercase(), Some(v.trim().to_string())),
                    None => (a.to_ascii_lowercase(), None),
                }
            })
            .filter(|(k, _)| !k.is_empty())
            .collect();
        Some(SetCookie {
            name: name.trim().to_string(),
            value: value.trim().to_string(),
            attrs,
        })
    }

    pub fn has(&self, attr: &str) -> bool {
        self.attrs.iter().any(|(k, _)| k == attr)
    }

    pub fn attr(&self, attr: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == attr)
            .and_then(|(_, v)| v.as_deref())
    }
}

#[derive(Clone, Debug)]
struct Stored {
    name: String,
    value: String,
    /// The host of the answer that set it (host-only) or the `Domain` value.
    domain: String,
    host_only: bool,
    path: String,
}

/// A cookie jar that follows browser rules (RFC 6265bis): host-only cookies go back only to the
/// same host, `Domain` cookies to the domain and its subdomains, `__Host-` cookies are refused
/// unless `Secure`, `Path=/` and without `Domain`, `__Secure-` cookies unless `Secure`, and a
/// `Max-Age` of zero or less removes a cookie. The harness speaks for an `https` origin. It has
/// no Public Suffix List: a `Domain` cookie for a public suffix (`ruumapps.net`) would be kept,
/// where a browser drops it. The cases never rely on that (the gateway sets no `Domain`).
#[derive(Clone, Debug, Default)]
pub struct Jar {
    cookies: Vec<Stored>,
}

impl Jar {
    /// Stores the `Set-Cookie` header `raw` of an answer from `host`. Returns whether it was
    /// kept.
    pub fn store(&mut self, host: &str, raw: &str) -> bool {
        let Some(c) = SetCookie::parse(raw) else {
            return false;
        };
        if c.name.is_empty() {
            return false;
        }
        let host = host.to_ascii_lowercase();
        let lower = c.name.to_ascii_lowercase();
        let domain_attr = c
            .attr("domain")
            .map(|d| d.trim_start_matches('.').to_ascii_lowercase());
        let path = c.attr("path").unwrap_or("/").to_string();
        if lower.starts_with("__host-")
            && (!c.has("secure") || domain_attr.is_some() || path != "/")
        {
            return false;
        }
        if lower.starts_with("__secure-") && !c.has("secure") {
            return false;
        }
        let (domain, host_only) = match domain_attr {
            Some(d) => {
                if !(host == d || host.ends_with(&format!(".{d}"))) {
                    return false;
                }
                (d, false)
            }
            None => (host.clone(), true),
        };
        self.cookies
            .retain(|s| !(s.name == c.name && s.domain == domain && s.path == path));
        let expired = c
            .attr("max-age")
            .and_then(|v| v.parse::<i64>().ok())
            .map(|v| v <= 0)
            .unwrap_or(false);
        if expired {
            return true;
        }
        self.cookies.push(Stored {
            name: c.name,
            value: c.value,
            domain,
            host_only,
            path,
        });
        true
    }

    fn matching(&self, host: &str, path: &str) -> Vec<&Stored> {
        let host = host.to_ascii_lowercase();
        self.cookies
            .iter()
            .filter(|s| {
                let domain_ok = if s.host_only {
                    s.domain == host
                } else {
                    host == s.domain || host.ends_with(&format!(".{}", s.domain))
                };
                let path_ok = path == s.path
                    || (path.starts_with(&s.path)
                        && (s.path.ends_with('/') || path[s.path.len()..].starts_with('/')));
                domain_ok && path_ok
            })
            .collect()
    }

    /// The `Cookie` header a browser sends to `host` for `path`.
    pub fn header_for(&self, host: &str, path: &str) -> Option<String> {
        let m = self.matching(host, path);
        if m.is_empty() {
            return None;
        }
        Some(
            m.iter()
                .map(|s| format!("{}={}", s.name, s.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// The value of cookie `name` that goes to `host`.
    pub fn value(&self, host: &str, name: &str) -> Option<String> {
        self.matching(host, "/")
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.value.clone())
    }

    /// The names of the cookies that go to `host`.
    pub fn names(&self, host: &str) -> Vec<String> {
        self.matching(host, "/")
            .into_iter()
            .map(|s| s.name.clone())
            .collect()
    }

    /// Every stored value (for redaction).
    pub fn values(&self) -> Vec<String> {
        self.cookies.iter().map(|s| s.value.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chunked_and_length_answers() {
        let r = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
            false,
        )
        .unwrap();
        assert_eq!(r.body, b"abcde");
        assert_eq!(r.set_cookies(), vec!["a=1", "b=2"]);
        let r = parse_response(
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 404 Not Found\r\ncontent-length: 2\r\n\r\nno",
            false,
        )
        .unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, &b"no"[..]));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nab", false).is_err());
        assert!(parse_response(b"", false).is_err());
    }

    #[test]
    fn the_jar_follows_browser_rules() {
        let mut j = Jar::default();
        let h = "garden.acme.ruumapps.localhost";
        assert!(j.store(
            h,
            "__Host-ruum_app=s1; Path=/; Secure; HttpOnly; SameSite=Lax"
        ));
        assert!(!j.store(h, "__Host-x=1; Path=/; Domain=ruumapps.localhost; Secure"));
        assert!(!j.store(h, "__Host-y=1; Path=/"));
        assert!(!j.store(h, "__Secure-z=1"));
        assert!(!j.store(h, "=ruum_app=x"));
        assert!(!j.store(h, "nameless"));
        assert!(!j.store(h, "d=1; Domain=other.localhost"));
        assert!(j.store(h, "wide=1; Domain=acme.ruumapps.localhost"));
        assert_eq!(j.value(h, "__Host-ruum_app").as_deref(), Some("s1"));
        assert_eq!(
            j.header_for("shed.acme.ruumapps.localhost", "/").as_deref(),
            Some("wide=1")
        );
        assert!(
            j.header_for("garden.globex.ruumapps.localhost", "/")
                .is_none()
        );
        assert!(j.store(h, "__Host-ruum_app=; Path=/; Secure; Max-Age=0"));
        assert_eq!(j.value(h, "__Host-ruum_app"), None);
    }
}
