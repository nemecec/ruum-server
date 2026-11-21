//! Static files (plan seam 1, `ruum_core::apps::static_site`; decision 0006 G point 4; backlog
//! S-38; decision 0008-4).
//!
//! The gateway holds **no key of the static sites' bucket** (decision 0008-4). It refuses a bad
//! path first (`request_path`: an empty segment, `.`, `..`, a `%`, a `\`, a control byte: `400`),
//! then asks the core ([`StaticSource`], the core's `static/file`): the core checks that the
//! deployment serves, looks the path up in its site index (an exact match, then
//! `<path>/index.html`, then `spa_fallback` for a navigation) and answers with the index entry
//! and a read link of that one object, signed for 30 seconds. A miss is the gateway's own `404`.
//! The gateway fetches the file through the link and streams it with a size limit, never read
//! whole and never longer than the index says. `ETag` is the file's hash and `Content-Type`
//! comes from the index. A link is a credential while it lasts: it is never logged. The clients
//! never use a proxy from the environment (Kimi review of run 14, R-3 finding 1).

use crate::core_api::{CoreError, CoreResult};
use crate::{Gateway, Site, pages};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use ruum_core::apps::static_site::wire::{StaticContentQuery, StaticFileAnswer, StaticFileQuery};
use ruum_core::apps::static_site::{IndexEntry, request_path};
use ruum_core::identity::app_sign_in::wire::PREFIX;
use std::time::Duration;

/// The largest static file the gateway serves (the plan's file size limit is smaller).
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// The bytes of a static file as the gateway streams them.
pub type FileStream = BoxStream<'static, Result<Bytes, std::io::Error>>;

/// Where the gateway gets static files: the core (decision 0008-4).
#[async_trait]
pub trait StaticSource: Send + Sync {
    /// The entry of a request path and a read link of its object.
    async fn file(&self, q: &StaticFileQuery) -> CoreResult<StaticFileAnswer>;
    /// Opens the file of `entry`: through `link`, or from the core when there is none.
    /// `Ok(None)` when the object is gone. The stream never yields more than `entry.size`
    /// bytes, and a length that differs from the entry is an error.
    async fn open(
        &self,
        q: &StaticFileQuery,
        entry: &IndexEntry,
        link: Option<&str>,
    ) -> CoreResult<Option<FileStream>>;
}

/// No static files at all (every path is the gateway's `404`).
pub struct NoStatic;

#[async_trait]
impl StaticSource for NoStatic {
    async fn file(&self, _: &StaticFileQuery) -> CoreResult<StaticFileAnswer> {
        Ok(StaticFileAnswer::NotFound)
    }
    async fn open(
        &self,
        _: &StaticFileQuery,
        _: &IndexEntry,
        _: Option<&str>,
    ) -> CoreResult<Option<FileStream>> {
        Ok(None)
    }
}

/// The core's internal listener (`RUUM_GATEWAY_INTERNAL_URL`) and the links it signs.
pub struct CoreStatic {
    base: String,
    client: reqwest::Client,
    /// For file bytes: no total timeout, a timeout per read instead.
    streams: reqwest::Client,
}

impl CoreStatic {
    /// A client of the internal listener at `base` (`http://ruum-server:8081`).
    pub fn new(base: &str) -> CoreResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        let streams = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            client,
            streams,
        })
    }

    /// The bytes of an answer of the bucket or the core, checked against `entry`.
    fn checked(
        r: reqwest::Response,
        entry: &IndexEntry,
        what: &str,
    ) -> CoreResult<Option<FileStream>> {
        let status = r.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status != reqwest::StatusCode::OK {
            return Err(CoreError(format!("{what}: status {status}")));
        }
        if let Some(n) = r.content_length()
            && n != entry.size
        {
            return Err(CoreError(format!(
                "{what}: the object does not have the size of its index entry"
            )));
        }
        let limit = entry.size;
        let mut seen = 0u64;
        Ok(Some(
            r.bytes_stream()
                .map(move |chunk| {
                    let chunk = chunk.map_err(|e| std::io::Error::other(e.without_url()))?;
                    seen += chunk.len() as u64;
                    if seen > limit {
                        return Err(std::io::Error::other(
                            "a static file object is longer than its index entry",
                        ));
                    }
                    Ok(chunk)
                })
                .boxed(),
        ))
    }
}

#[async_trait]
impl StaticSource for CoreStatic {
    async fn file(&self, q: &StaticFileQuery) -> CoreResult<StaticFileAnswer> {
        let r = self
            .client
            .post(format!("{}{PREFIX}/static/file", self.base))
            .json(q)
            .send()
            .await
            .map_err(|e| CoreError(format!("/static/file: {}", e.without_url())))?;
        if !r.status().is_success() {
            return Err(CoreError(format!("/static/file: status {}", r.status())));
        }
        r.json()
            .await
            .map_err(|e| CoreError(format!("/static/file: {}", e.without_url())))
    }

    async fn open(
        &self,
        q: &StaticFileQuery,
        entry: &IndexEntry,
        link: Option<&str>,
    ) -> CoreResult<Option<FileStream>> {
        if entry.size > MAX_FILE_BYTES {
            return Err(CoreError(format!(
                "the file is {} bytes, more than {MAX_FILE_BYTES}",
                entry.size
            )));
        }
        let r = match link {
            Some(link) => {
                let url = url::Url::parse(link)
                    .ok()
                    .filter(|u| matches!(u.scheme(), "https" | "http"))
                    .ok_or_else(|| CoreError("the core gave a link that is not a URL".into()))?;
                self.streams
                    .get(url)
                    .send()
                    .await
                    // Never the URL: its query is the signature.
                    .map_err(|e| CoreError(format!("a static file link: {}", e.without_url())))?
            }
            None => self
                .streams
                .post(format!("{}{PREFIX}/static/content", self.base))
                .json(&StaticContentQuery {
                    tenant_id: q.tenant_id,
                    app_id: q.app_id,
                    deployment_id: q.deployment_id,
                    hash: entry.hash.clone(),
                })
                .send()
                .await
                .map_err(|e| CoreError(format!("/static/content: {}", e.without_url())))?,
        };
        Self::checked(
            r,
            entry,
            if link.is_some() {
                "a static file link"
            } else {
                "/static/content"
            },
        )
    }
}

/// Whether a request is a browser navigation (`spa_fallback` answers only those).
pub fn is_navigation(method: &Method, headers: &HeaderMap) -> bool {
    if method != Method::GET && method != Method::HEAD {
        return false;
    }
    match headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok()) {
        Some(mode) => mode.eq_ignore_ascii_case("navigate"),
        None => headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|a| a.contains("text/html")),
    }
}

/// Serves a static file of the site's deployment (the security headers are added here).
pub async fn serve(
    gw: &Gateway,
    site: &Site,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
) -> Response {
    let frame = site.frame();
    if method != Method::GET && method != Method::HEAD {
        let mut r = pages::page(
            StatusCode::METHOD_NOT_ALLOWED,
            "This address takes no such request.",
            frame,
            &gw.config.ui_origin,
        );
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return r;
    }
    if request_path(path).is_none() {
        return pages::bad_request(site.look());
    }
    let q = StaticFileQuery {
        tenant_id: site.resolved.tenant_id,
        app_id: site.resolved.app_id,
        deployment_id: site.resolved.deployment_id,
        path: path.to_string(),
        navigation: is_navigation(method, headers),
    };
    let (entry, link) = match gw.statics.file(&q).await {
        Ok(StaticFileAnswer::Found { entry, link }) => (entry, link),
        Ok(StaticFileAnswer::NotFound | StaticFileAnswer::NotServing) => {
            return pages::not_found(site.look());
        }
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "a static file could not be looked up");
            return pages::unavailable(site.look());
        }
    };
    let etag = format!("\"{}\"", entry.hash);
    let mut r = Response::new(Body::empty());
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(v) = HeaderValue::from_str(&entry.media_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    crate::proxy::security_headers(h, frame, &gw.config.ui_origin);
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if not_modified {
        *r.status_mut() = StatusCode::NOT_MODIFIED;
        return r;
    }
    r.headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(entry.size));
    if method == Method::HEAD {
        return r;
    }
    let stream = match gw.statics.open(&q, &entry, link.as_deref()).await {
        Ok(Some(s)) => s,
        Ok(None) => return pages::not_found(site.look()),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "a static file could not be opened");
            return pages::unavailable(site.look());
        }
    };
    *r.body_mut() = Body::from_stream(stream);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigations_are_top_level_html_requests() {
        let mut h = HeaderMap::new();
        h.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
        assert!(is_navigation(&Method::GET, &h));
        assert!(!is_navigation(&Method::POST, &h));
        h.insert("sec-fetch-mode", HeaderValue::from_static("no-cors"));
        h.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        assert!(!is_navigation(&Method::GET, &h));
        let mut h = HeaderMap::new();
        h.insert(header::ACCEPT, HeaderValue::from_static("text/html,*/*"));
        assert!(is_navigation(&Method::GET, &h));
        h.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        assert!(!is_navigation(&Method::GET, &h));
    }

    #[test]
    fn bad_segments_are_refused_before_any_lookup() {
        for bad in [
            "/sub/../index.html",
            "/../x",
            "/%2e%2e/x",
            "/sub%2fpage.html",
            "/sub\\page.html",
            "/sub%5cpage.html",
            "/sub/%00page.html",
            "/sub/page\u{1}.html",
            "//index.html",
            "/sub//page.html",
            "/./index.html",
            "/sub/./page.html",
            "/%252e%252e/x",
            "relative",
        ] {
            assert_eq!(request_path(bad), None, "{bad:?}");
        }
        assert_eq!(request_path("/"), Some(""));
        assert_eq!(request_path("/docs/"), Some("docs/"));
        assert_eq!(request_path("/css/site.css"), Some("css/site.css"));
        // `..;` is a name, not a traversal; it is not in any index, so the answer is 404.
        assert_eq!(request_path("/sub/..;/x"), Some("sub/..;/x"));
    }
}
