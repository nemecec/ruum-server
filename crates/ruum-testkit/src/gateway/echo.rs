//! The fake echo app: an in-process HTTP server that answers every request with the request it
//! got (method, target, every header, the cookies, the body) as JSON, and records it. A case asks
//! it for response headers (`Set-Cookie`, `Content-Security-Policy`) with [`Echo::reply_with`].
//! The world points a deployment at it with [`super::GatewayWorld::set_upstream`].

use super::ECHO_MARKER;
use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// One request as the echo app got it.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    /// The path and query as they arrived.
    pub target: String,
    /// Every header, in order, names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header_all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

#[derive(Default)]
struct State {
    seen: Vec<Seen>,
    reply: Vec<(String, String)>,
}

/// A running echo app. It stops when dropped.
pub struct Echo {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    pub addr: SocketAddr,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Echo {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The cookies of `Cookie` headers as `(name, value)` pairs, in order.
pub fn cookie_pairs<'a>(headers: impl IntoIterator<Item = &'a str>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .flat_map(|h| h.split(';'))
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            Some(match p.split_once('=') {
                Some((n, v)) => (n.trim().to_string(), v.trim().to_string()),
                None => (String::new(), p.to_string()),
            })
        })
        .collect()
}

impl Echo {
    /// Starts the echo app on a free port of 127.0.0.1.
    pub async fn start() -> Echo {
        let state = Arc::new(Mutex::new(State::default()));
        let st = state.clone();
        let router = Router::new().fallback(move |req: Request| {
            let st = st.clone();
            async move { answer(st, req).await }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the echo app");
        let addr = listener.local_addr().expect("echo address");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Echo {
            url: format!("http://{addr}"),
            addr,
            state,
            task,
        }
    }

    /// Every request the app got so far.
    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().expect("lock").seen.clone()
    }

    /// How many requests the app got.
    pub fn hits(&self) -> usize {
        self.state.lock().expect("lock").seen.len()
    }

    /// The headers that every later answer sends (a name may repeat: several `Set-Cookie`).
    pub fn reply_with(&self, headers: &[(&str, &str)]) {
        self.state.lock().expect("lock").reply = headers
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
    }
}

async fn answer(st: Arc<Mutex<State>>, req: Request) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let body = axum::body::to_bytes(body, 16 * 1024 * 1024)
        .await
        .map(|b| b.to_vec())
        .unwrap_or_default();
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .map(|(n, v)| {
            (
                n.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.to_string());
    let cookies = cookie_pairs(
        headers
            .iter()
            .filter(|(n, _)| n == "cookie")
            .map(|(_, v)| v.as_str()),
    );
    let seen = Seen {
        method: parts.method.as_str().to_string(),
        target: target.clone(),
        headers: headers.clone(),
        body: body.clone(),
    };
    let reply = {
        let mut s = st.lock().expect("lock");
        s.seen.push(seen);
        s.reply.clone()
    };
    let v: Value = json!({
        "echo": ECHO_MARKER,
        "method": parts.method.as_str(),
        "target": target,
        "headers": headers.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
        "cookies": cookies.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
        "body": String::from_utf8_lossy(&body),
    });
    let mut resp = Response::new(Body::from(v.to_string()));
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    for (n, v) in reply {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(n.as_bytes()),
            HeaderValue::from_str(&v),
        ) {
            h.append(n, v);
        }
    }
    resp
}

// ---------------------------------------------------------------------------------------------
// WebSockets (card G-1b)
// ---------------------------------------------------------------------------------------------

use super::http::{Req, Resp, parse_response};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The `Sec-WebSocket-Accept` of a key (RFC 6455 4.2.2): base64 of SHA-1 of the key and the
/// fixed GUID.
pub fn websocket_accept(key: &str) -> String {
    use base64::Engine as _;
    let mut input = key.trim().as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::engine::general_purpose::STANDARD.encode(sha1(&input))
}

/// SHA-1 (FIPS 180-4), only for the WebSocket handshake of the test apps.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, x) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

/// One WebSocket frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WsFrame {
    pub opcode: u8,
    pub payload: Vec<u8>,
}

impl WsFrame {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.payload).into_owned()
    }

    /// The status code of a close frame.
    pub fn close_code(&self) -> Option<u16> {
        (self.opcode == 8 && self.payload.len() >= 2)
            .then(|| u16::from_be_bytes([self.payload[0], self.payload[1]]))
    }
}

/// Writes one frame; `mask` for the client side (RFC 6455 5.3).
fn encode_frame(opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = vec![0x80 | (opcode & 0x0f)];
    let m = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(m | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(m | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(m | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(k) => {
            out.extend_from_slice(&k);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ k[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

/// Reads one frame from `buf` (bytes already read) and `stream`. `Ok(None)`: the connection
/// ended.
async fn read_frame(stream: &mut TcpStream, buf: &mut Vec<u8>) -> std::io::Result<Option<WsFrame>> {
    loop {
        if buf.len() >= 2 {
            let short = buf[1] & 0x7f;
            let masked = buf[1] & 0x80 != 0;
            let ext = match short {
                126 => 2,
                127 => 8,
                _ => 0,
            };
            if buf.len() >= 2 + ext {
                let len = match short {
                    126 => u16::from_be_bytes([buf[2], buf[3]]) as usize,
                    127 => u64::from_be_bytes(buf[2..10].try_into().expect("8 bytes")) as usize,
                    n => n as usize,
                };
                let head = 2 + ext + if masked { 4 } else { 0 };
                if buf.len() >= head + len {
                    let opcode = buf[0] & 0x0f;
                    let mut payload = buf[head..head + len].to_vec();
                    if masked {
                        let k = [buf[head - 4], buf[head - 3], buf[head - 2], buf[head - 1]];
                        for (i, b) in payload.iter_mut().enumerate() {
                            *b ^= k[i % 4];
                        }
                    }
                    buf.drain(..head + len);
                    return Ok(Some(WsFrame { opcode, payload }));
                }
            }
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Reads an HTTP head (up to the empty line) and returns it and the bytes after it.
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(i + 4);
            return Ok(Some((buf, rest)));
        }
        if buf.len() > 64 * 1024 {
            return Ok(None);
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[derive(Default)]
struct WsState {
    seen: Vec<Seen>,
    upgrades: usize,
}

/// A WebSocket echo app on its own port. It records every request (also each upgrade, as a
/// [`Seen`]); an upgrade gets `101` and every text or binary frame comes back unchanged; any
/// other request gets the JSON answer of [`Echo`] and the connection closes. It stops when
/// dropped (its open connections too).
pub struct WsEcho {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    pub addr: SocketAddr,
    state: Arc<Mutex<WsState>>,
    tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Drop for WsEcho {
    fn drop(&mut self) {
        for t in self.tasks.lock().expect("lock").drain(..) {
            t.abort();
        }
    }
}

impl WsEcho {
    pub async fn start() -> WsEcho {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the WebSocket echo app");
        let addr = listener.local_addr().expect("address");
        let state = Arc::new(Mutex::new(WsState::default()));
        let tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> = Arc::default();
        let (st, ts) = (state.clone(), tasks.clone());
        let accept = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let st = st.clone();
                let t = tokio::spawn(async move {
                    let _ = ws_connection(stream, st).await;
                });
                ts.lock().expect("lock").push(t);
            }
        });
        tasks.lock().expect("lock").push(accept);
        WsEcho {
            url: format!("http://{addr}"),
            addr,
            state,
            tasks,
        }
    }

    /// Every request so far (upgrades and plain requests).
    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().expect("lock").seen.clone()
    }

    pub fn hits(&self) -> usize {
        self.state.lock().expect("lock").seen.len()
    }

    /// How many upgrades the app accepted.
    pub fn upgrades(&self) -> usize {
        self.state.lock().expect("lock").upgrades
    }
}

async fn ws_connection(mut stream: TcpStream, st: Arc<Mutex<WsState>>) -> std::io::Result<()> {
    let Some((head, mut rest)) = read_head(&mut stream).await? else {
        return Ok(());
    };
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let upgrade = get("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let mut seen = Seen {
        method: method.clone(),
        target: target.clone(),
        headers: headers.clone(),
        body: Vec::new(),
    };
    if !upgrade {
        let len: usize = get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while rest.len() < len {
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            rest.extend_from_slice(&chunk[..n]);
        }
        seen.body = rest.clone();
        st.lock().expect("lock").seen.push(seen);
        let v: Value = json!({
            "echo": ECHO_MARKER,
            "method": method,
            "target": target,
            "headers": headers.iter().map(|(n, v)| json!([n, v])).collect::<Vec<_>>(),
            "cookies": [],
            "body": String::from_utf8_lossy(&rest),
        });
        let body = v.to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(resp.as_bytes()).await?;
        return stream.shutdown().await;
    }
    {
        let mut s = st.lock().expect("lock");
        s.seen.push(seen);
        s.upgrades += 1;
    }
    let accept = websocket_accept(&get("sec-websocket-key").unwrap_or_default());
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(resp.as_bytes()).await?;
    while let Some(frame) = read_frame(&mut stream, &mut rest).await? {
        match frame.opcode {
            1 | 2 => {
                stream
                    .write_all(&encode_frame(frame.opcode, &frame.payload, None))
                    .await?
            }
            9 => {
                stream
                    .write_all(&encode_frame(10, &frame.payload, None))
                    .await?
            }
            8 => {
                stream
                    .write_all(&encode_frame(8, &frame.payload, None))
                    .await?;
                break;
            }
            _ => {}
        }
    }
    stream.shutdown().await
}

/// The client side of a WebSocket after a `101`.
pub struct WsClient {
    stream: TcpStream,
    buf: Vec<u8>,
}

/// What a WebSocket handshake through the gateway gave.
pub enum WsOpen {
    /// `101`: the head of the answer and the open connection.
    Open(Resp, WsClient),
    /// Any other answer, read whole.
    Refused(Resp),
}

/// Sends `req` (with its own `Upgrade`, `Connection` and `Sec-WebSocket-*` headers) to `addr`
/// and reads the answer's head. A connection error or a timeout is an error, never a refusal.
pub async fn ws_open(addr: SocketAddr, req: &Req, timeout: Duration) -> Result<WsOpen, String> {
    tokio::time::timeout(timeout, async {
        let mut stream = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect {addr}: {e}"))?;
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\n",
            req.method, req.target, req.host
        );
        for (n, v) in &req.headers {
            head.push_str(&format!("{n}: {v}\r\n"));
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(|e| format!("write: {e}"))?;
        let (h, rest) = read_head(&mut stream)
            .await
            .map_err(|e| format!("read: {e}"))?
            .ok_or("the connection closed without an answer")?;
        if h.starts_with(b"HTTP/1.1 101 ") {
            // `parse_response` skips 1xx answers; read the head as a final one.
            let mut head = b"HTTP/1.1 299 ".to_vec();
            head.extend_from_slice(&h[13..]);
            let mut resp = parse_response(&head, true)?;
            resp.status = 101;
            return Ok(WsOpen::Open(resp, WsClient { stream, buf: rest }));
        }
        let mut all = h;
        all.extend_from_slice(&rest);
        let _ = stream.read_to_end(&mut all).await;
        Ok(WsOpen::Refused(parse_response(&all, false)?))
    })
    .await
    .map_err(|_| format!("timeout after {} s", timeout.as_secs()))?
}

impl WsClient {
    /// Sends a masked text frame.
    pub async fn send_text(&mut self, text: &str) -> Result<(), String> {
        let mask: [u8; 4] = rand_mask();
        self.stream
            .write_all(&encode_frame(1, text.as_bytes(), Some(mask)))
            .await
            .map_err(|e| format!("write: {e}"))
    }

    /// The next frame, `Ok(None)` when the connection ended, `Err` on a timeout.
    pub async fn recv(&mut self, timeout: Duration) -> Result<Option<WsFrame>, String> {
        match tokio::time::timeout(timeout, read_frame(&mut self.stream, &mut self.buf)).await {
            Err(_) => Err(format!("no frame within {} ms", timeout.as_millis())),
            Ok(Ok(f)) => Ok(f),
            // A reset ends the connection too.
            Ok(Err(_)) => Ok(None),
        }
    }
}

fn rand_mask() -> [u8; 4] {
    let n = uuid::Uuid::now_v7().as_u128();
    (n as u32).to_be_bytes()
}

#[cfg(test)]
mod ws_tests {
    use super::*;

    #[test]
    fn the_accept_value_follows_rfc_6455() {
        // The example of RFC 6455 1.3.
        assert_eq!(
            websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(
            sha1(b"abc"),
            [
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
            ]
        );
    }

    #[tokio::test]
    async fn the_websocket_echo_answers_frames_and_plain_requests() {
        let e = WsEcho::start().await;
        let req = Req::get("app.test", "/ws")
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
        let WsOpen::Open(head, mut c) =
            ws_open(e.addr, &req, Duration::from_secs(5)).await.unwrap()
        else {
            panic!("no 101");
        };
        assert_eq!(
            head.header("sec-websocket-accept"),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
        c.send_text("hello").await.unwrap();
        let f = c.recv(Duration::from_secs(5)).await.unwrap().unwrap();
        assert_eq!((f.opcode, f.text()), (1, "hello".to_string()));
        let r =
            super::super::http::send(e.addr, &Req::get("app.test", "/x"), Duration::from_secs(5))
                .await
                .unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(e.hits(), 2);
        assert_eq!(e.upgrades(), 1);
    }
}
