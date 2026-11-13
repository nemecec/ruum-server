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
