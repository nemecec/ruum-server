//! Static files (plan seam 1, `ruum_core::apps::static_site`; decision 0006 G point 4; backlog
//! S-38; decision 0008-4).
//!
//! The gateway holds **no object store key** (decision 0008-4). For each request it refuses a
//! bad path first (`request_path`: an empty segment, `.`, `..`, a `%`, a `\`, a control byte:
//! `400`), then asks the core's internal listener for the file of that path
//! ([`SiteFiles::link`], `POST /internal/gateway/v1/static/file`). The core checks that the
//! deployment serves and is static, looks the path up in the deployment's site index (an exact
//! match, then `<path>/index.html`, then `spa_fallback` for a navigation) and answers with the
//! file's hash, size and media type and a link that reads that one object for a minute, signed
//! with the core's key; a miss is the gateway's own `404`. The gateway never builds an object
//! key. The file streams from the object store through the link with a size limit, never read
//! whole. `ETag` is the file's hash and `Content-Type` comes from the index. A `HEAD` and an
//! `If-None-Match` that matches fetch no object.

use crate::core_api::{CoreError, CoreResult};
use crate::{Gateway, Site, pages};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use ruum_core::apps::static_site::request_path;
use ruum_core::identity::app_sign_in::wire::{PREFIX, StaticFileLink, StaticFileQuery};
use std::time::Duration;

/// The largest static file the gateway serves (the plan's file size limit is smaller).
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// How long the gateway waits for the object store to start an answer.
pub const OBJECT_FIRST_BYTE: Duration = Duration::from_secs(20);

/// What the core answers for a path of a static site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkAnswer {
    /// The file and a short-lived link to its object.
    File(StaticFileLink),
    /// No file for the path, or the deployment does not serve a static site.
    NotFound,
    /// The core has no static store that can sign links (the server without S3).
    Unavailable,
}

/// The bytes of a static file as they arrive.
pub type FileStream = BoxStream<'static, Result<Bytes, std::io::Error>>;

/// Where the gateway gets static files: the core's link ([`SiteFiles::link`]), then the object
/// through it ([`SiteFiles::open`]).
#[async_trait]
pub trait SiteFiles: Send + Sync {
    async fn link(&self, q: &StaticFileQuery) -> CoreResult<LinkAnswer>;
    /// Opens the object of `link` (`GET`, no redirect). Fails when the answer is not `200`,
    /// when its length is not `link.size`, and stops a stream that delivers more.
    async fn open(&self, link: &StaticFileLink) -> CoreResult<FileStream>;
}

/// [`SiteFiles`] over HTTP: the core's internal listener for links, the object store for bytes.
pub struct HttpSiteFiles {
    base: String,
    core: reqwest::Client,
    objects: reqwest::Client,
}

impl HttpSiteFiles {
    /// Asks the internal listener at `base` (`RUUM_GATEWAY_INTERNAL_URL`).
    pub fn new(base: &str) -> CoreResult<Self> {
        let core = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        let objects = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(OBJECT_FIRST_BYTE)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CoreError(e.to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            core,
            objects,
        })
    }
}

#[async_trait]
impl SiteFiles for HttpSiteFiles {
    async fn link(&self, q: &StaticFileQuery) -> CoreResult<LinkAnswer> {
        let r = self
            .core
            .post(format!("{}{PREFIX}/static/file", self.base))
            .json(q)
            .send()
            .await
            .map_err(|e| CoreError(format!("/static/file: {}", e.without_url())))?;
        match r.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(LinkAnswer::NotFound),
            reqwest::StatusCode::SERVICE_UNAVAILABLE => Ok(LinkAnswer::Unavailable),
            s if s.is_success() => r
                .json::<StaticFileLink>()
                .await
                .map(LinkAnswer::File)
                .map_err(|e| CoreError(format!("/static/file: {}", e.without_url()))),
            s => Err(CoreError(format!("/static/file: status {s}"))),
        }
    }

    async fn open(&self, link: &StaticFileLink) -> CoreResult<FileStream> {
        let url = url::Url::parse(&link.url)
            .ok()
            .filter(|u| matches!(u.scheme(), "https" | "http"))
            .ok_or_else(|| CoreError("a static file link is not an http(s) URL".into()))?;
        // The link's query is its signature: errors drop the URL, never print it.
        let r = self
            .objects
            .get(url)
            .send()
            .await
            .map_err(|e| CoreError(format!("static object: {}", e.without_url())))?;
        if r.status() != reqwest::StatusCode::OK {
            return Err(CoreError(format!("static object: status {}", r.status())));
        }
        if r.content_length().is_some_and(|n| n != link.size) {
            return Err(CoreError(
                "a static object does not have the size of its index entry".into(),
            ));
        }
        let limit = link.size;
        let mut seen = 0u64;
        Ok(r.bytes_stream()
            .map(move |chunk| {
                let chunk = chunk.map_err(|e| std::io::Error::other(e.without_url()))?;
                seen += chunk.len() as u64;
                if seen > limit {
                    return Err(std::io::Error::other(
                        "a static object is longer than its index entry",
                    ));
                }
                Ok(chunk)
            })
            .boxed())
    }
}

/// No static files: every path answers the `503` page (a gateway without a core in tests).
pub struct NoSiteFiles;

#[async_trait]
impl SiteFiles for NoSiteFiles {
    async fn link(&self, _: &StaticFileQuery) -> CoreResult<LinkAnswer> {
        Ok(LinkAnswer::Unavailable)
    }

    async fn open(&self, _: &StaticFileLink) -> CoreResult<FileStream> {
        Err(CoreError("no static files".into()))
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
    let Some(path) = request_path(path) else {
        return pages::bad_request(site.look());
    };
    let q = StaticFileQuery {
        tenant_id: site.resolved.tenant_id,
        app_id: site.resolved.app_id,
        deployment_id: site.resolved.deployment_id,
        path: path.to_string(),
        navigation: is_navigation(method, headers),
    };
    let link = match gw.sites.link(&q).await {
        Ok(LinkAnswer::File(l)) => l,
        Ok(LinkAnswer::NotFound) => return pages::not_found(site.look()),
        Ok(LinkAnswer::Unavailable) => {
            tracing::warn!(target: "ruum::gateway", "the core serves no static files (no static store)");
            return pages::unavailable(site.look());
        }
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "a static file could not be looked up");
            return pages::unavailable(site.look());
        }
    };
    if link.size > MAX_FILE_BYTES {
        tracing::warn!(target: "ruum::gateway", size = link.size, "a static file is too large to serve");
        return pages::unavailable(site.look());
    }
    let etag = format!("\"{}\"", link.hash);
    let mut r = Response::new(Body::empty());
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Ok(v) = HeaderValue::from_str(&link.media_type) {
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
        .insert(header::CONTENT_LENGTH, HeaderValue::from(link.size));
    if method == Method::HEAD {
        return r;
    }
    let stream = match gw.sites.open(&link).await {
        Ok(s) => s,
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
