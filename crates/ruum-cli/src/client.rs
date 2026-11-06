//! The HTTP client of the CLI: the agent API key, the token exchange, and requests with the
//! standard Ruum error body.
//!
//! The key is exchanged at `POST /v1/oauth/token` (RFC 8693 token exchange) for a 15-minute
//! access token. The CLI keeps the key and the current token in the credentials file (mode
//! 0600) and exchanges the key again when the token is about to end or the API answers `401`.

use anyhow::{Context as _, Result, anyhow};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The prefix of agent API keys.
pub const API_KEY_PREFIX: &str = "ruum_agent_";
const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const API_KEY_TOKEN_TYPE: &str = "urn:ruum:params:oauth:token-type:api-key";
/// A token is exchanged again this many seconds before it ends.
const TOKEN_MARGIN: u64 = 60;

/// What the credentials file holds.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Credentials {
    pub url: String,
    pub api_key: String,
    #[serde(default)]
    pub access_token: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl Credentials {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| {
                format!("the credentials file {} is not valid", path.display())
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Writes the file, readable only by the user.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("tmp");
        {
            use std::io::Write as _;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                opts.mode(0o600);
            }
            let mut f = opts
                .open(&tmp)
                .with_context(|| format!("write {}", tmp.display()))?;
            f.write_all(text.as_bytes())?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

/// The default credentials file: `$XDG_CONFIG_HOME/ruum/credentials.json` or
/// `~/.config/ruum/credentials.json`.
pub fn default_credentials_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ruum").join("credentials.json")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// An error answer of the API: `{ "error": { "code", "message", "hint" } }`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(h) = &self.hint {
            write!(f, "\nhint: {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ApiError {}

impl ApiError {
    async fn from_response(res: reqwest::Response) -> Self {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let e = &v["error"];
        if let Some(code) = e["code"].as_str() {
            return Self {
                status,
                code: code.to_string(),
                message: e["message"].as_str().unwrap_or_default().to_string(),
                hint: e["hint"].as_str().map(str::to_string),
            };
        }
        // OAuth errors of the token endpoint (`OAuthError`):
        // `{ "error": "invalid_grant", "error_description": ..., "ruum_code": "agent_disabled" }`.
        // The Ruum code is the machine-readable code; the OAuth error stays in the message.
        if let Some(oauth) = e.as_str() {
            let description = v["error_description"]
                .as_str()
                .unwrap_or("the token request failed");
            let (code, message) = match v["ruum_code"].as_str() {
                Some(ruum) => (ruum.to_string(), format!("{description} ({oauth})")),
                None => (oauth.to_string(), description.to_string()),
            };
            let hint = match code.as_str() {
                "agent_disabled" => "ask your user to enable the agent in the web UI (Agents)",
                _ => "check the API key (ruum login --key ...)",
            };
            return Self {
                status,
                code,
                message,
                hint: Some(hint.into()),
            };
        }
        Self {
            status,
            code: format!("http_{}", status.as_u16()),
            message: if text.is_empty() {
                status.to_string()
            } else {
                text.chars().take(300).collect()
            },
            hint: None,
        }
    }
}

/// The body of a request.
#[derive(Clone, Debug)]
pub enum Body {
    None,
    Json(Value),
    Bytes(Vec<u8>),
}

/// A signed-in client.
pub struct Client {
    http: reqwest::Client,
    creds: Credentials,
    /// Where the token is saved; `None` keeps it in memory (a key from the command line).
    path: Option<PathBuf>,
}

impl Client {
    /// A client for `url` with `api_key`. Nothing is sent yet.
    pub fn new(url: &str, api_key: &str, path: Option<PathBuf>) -> Result<Self> {
        let url = check_url(url)?;
        if !api_key.starts_with(API_KEY_PREFIX) {
            return Err(anyhow!(
                "the API key must start with {API_KEY_PREFIX}\nhint: create a key for your agent in the web UI (Agents > Keys)"
            ));
        }
        Ok(Self {
            http: http_client()?,
            creds: Credentials {
                url,
                api_key: api_key.to_string(),
                access_token: None,
                expires_at: None,
            },
            path,
        })
    }

    /// A client from saved credentials (the cached token is reused while it is valid).
    pub fn from_credentials(mut creds: Credentials, path: PathBuf) -> Result<Self> {
        creds.url = check_url(&creds.url)?;
        Ok(Self {
            http: http_client()?,
            creds,
            path: Some(path),
        })
    }

    pub fn url(&self) -> &str {
        &self.creds.url
    }

    pub fn credentials(&self) -> &Credentials {
        &self.creds
    }

    /// Saves the credentials (the key and the current token) to `path` from now on.
    pub fn persist_to(&mut self, path: PathBuf) -> Result<()> {
        self.creds.save(&path)?;
        self.path = Some(path);
        Ok(())
    }

    /// Exchanges the key for a new access token and saves it.
    pub async fn exchange(&mut self) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/v1/oauth/token", self.creds.url))
            .form(&[
                ("grant_type", GRANT_TOKEN_EXCHANGE),
                ("subject_token", self.creds.api_key.as_str()),
                ("subject_token_type", API_KEY_TOKEN_TYPE),
            ])
            .send()
            .await
            .with_context(|| format!("connect to {}", self.creds.url))?;
        if !res.status().is_success() {
            return Err(ApiError::from_response(res).await.into());
        }
        let v: Value = res.json().await.context("read the token response")?;
        let token = v["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("the token response has no access_token"))?;
        let expires_in = v["expires_in"].as_u64().unwrap_or(900);
        self.creds.access_token = Some(token.to_string());
        self.creds.expires_at = Some(now() + expires_in);
        if let Some(p) = &self.path {
            self.creds.save(p)?;
        }
        Ok(())
    }

    async fn token(&mut self) -> Result<String> {
        let valid = matches!(
            (&self.creds.access_token, self.creds.expires_at),
            (Some(_), Some(exp)) if exp > now() + TOKEN_MARGIN
        );
        if !valid {
            self.exchange().await?;
        }
        Ok(self.creds.access_token.clone().unwrap_or_default())
    }

    /// Sends a request. A `401` exchanges the key again and retries once. Writes carry a fresh
    /// `Idempotency-Key`, the same for the retry.
    pub async fn send(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Body,
        headers: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let idem = (method != Method::GET && method != Method::HEAD)
            .then(|| format!("cli-{}", uuid::Uuid::now_v7()));
        let mut retried = false;
        loop {
            let token = self.token().await?;
            let mut req = self
                .http
                .request(method.clone(), format!("{}{path}", self.creds.url))
                .bearer_auth(&token);
            if !query.is_empty() {
                req = req.query(query);
            }
            if let Some(k) = &idem {
                req = req.header("Idempotency-Key", k);
            }
            for (k, v) in headers {
                req = req.header(*k, v);
            }
            req = match &body {
                Body::None => req,
                Body::Json(v) => req.json(v),
                Body::Bytes(b) => req
                    .header("Content-Type", "application/octet-stream")
                    .body(b.clone()),
            };
            let res = req
                .send()
                .await
                .with_context(|| format!("connect to {}", self.creds.url))?;
            if res.status() == StatusCode::UNAUTHORIZED && !retried {
                retried = true;
                self.creds.access_token = None;
                continue;
            }
            if !res.status().is_success() {
                return Err(ApiError::from_response(res).await.into());
            }
            return Ok(res);
        }
    }

    /// Sends a request and reads the JSON answer (`null` for an empty body).
    pub async fn json(
        &mut self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Body,
    ) -> Result<Value> {
        let res = self.send(method, path, query, body, &[]).await?;
        let bytes = res.bytes().await?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).context("the API answer is not JSON")
    }

    pub async fn get(&mut self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.json(Method::GET, path, query, Body::None).await
    }

    pub async fn post(&mut self, path: &str, body: Value) -> Result<Value> {
        self.json(Method::POST, path, &[], Body::Json(body)).await
    }

    /// Every item of a paged list (`items`, `next_cursor`).
    pub async fn all_pages(&mut self, path: &str, query: &[(&str, String)]) -> Result<Vec<Value>> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = query.to_vec();
            if let Some(c) = &cursor {
                q.push(("cursor", c.clone()));
            }
            let page = self.get(path, &q).await?;
            if let Some(list) = page["items"].as_array() {
                items.extend(list.iter().cloned());
            }
            match page["next_cursor"].as_str() {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => return Ok(items),
            }
        }
    }
}

/// Normalizes an API base URL (no trailing `/`) and refuses one that would send the key in
/// clear text: `http://` is allowed only for loopback hosts and `*.localhost`.
pub fn check_url(url: &str) -> Result<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let parsed =
        reqwest::Url::parse(trimmed).map_err(|e| anyhow!("{trimmed} is not a valid URL: {e}"))?;
    match parsed.scheme() {
        "https" => {}
        "http" if is_local_host(&parsed) => {}
        "http" => {
            return Err(anyhow!(
                "{trimmed} uses http:// to a host that is not local; the API key would travel in clear text\nhint: use https://"
            ));
        }
        other => return Err(anyhow!("{trimmed}: the scheme {other} is not supported")),
    }
    if parsed.host().is_none() {
        return Err(anyhow!("{trimmed} has no host"));
    }
    Ok(trimmed.to_string())
}

fn is_local_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return ip.is_loopback();
    }
    let d = host.trim_end_matches('.').to_ascii_lowercase();
    d == "localhost" || d.ends_with(".localhost")
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("ruum-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build the HTTP client")
}

/// Percent-encodes one path segment (RFC 3986 unreserved characters stay).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
