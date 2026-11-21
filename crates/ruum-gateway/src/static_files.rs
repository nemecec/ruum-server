//! Static files (plan seam 1, `ruum_core::apps::static_site`; decision 0006 G point 4; backlog
//! S-38), read through links that the core signs (decision 0008-4; backlog I6-3D).
//!
//! The gateway holds no key of any bucket. It refuses a bad path first (`request_path`: an
//! empty segment, `.`, `..`, a `%`, a `\`, a control byte: `400`), then asks the core for the
//! file (`POST /internal/gateway/v1/static`). The core looks the path up in the deployment's
//! site index (an exact match, then `<path>/index.html`, then `spa_fallback` for a navigation)
//! and answers the file's hash, size and media type and a link that allows one `GET` of that
//! one file object for a minute; a miss is the gateway's own `404`. The gateway never builds an
//! object key or a link itself. The file streams from the link with a size limit, never read
//! whole, and a body longer or shorter than the index says stops. `ETag` is the file's hash and
//! `Content-Type` comes from the index. A link is never logged (it is a credential for a
//! minute).

use crate::{Gateway, Site, pages};
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use futures::StreamExt;
use ruum_core::apps::static_site::request_path;
use ruum_core::identity::app_sign_in::wire::StaticQuery;
use std::time::Duration;

/// The largest static file the gateway serves (the plan's file size limit is smaller).
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// The HTTP client of the links: no redirects (a link answers the file or an error), no proxy.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .expect("the HTTP client of static files builds")
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

/// A link of the core as a URL the client may fetch: `https`, or `http` (Garage on the laptop,
/// the tests). Anything else is refused.
fn link_url(link: &str) -> Option<reqwest::Url> {
    reqwest::Url::parse(link)
        .ok()
        .filter(|u| matches!(u.scheme(), "https" | "http") && u.host_str().is_some())
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
    let query = StaticQuery {
        tenant_id: site.resolved.tenant_id,
        app_id: site.resolved.app_id,
        deployment_id: site.resolved.deployment_id,
        path: path.to_string(),
        navigation: is_navigation(method, headers),
    };
    let entry = match gw.core.static_file(&query).await {
        Ok(Some(e)) => e,
        Ok(None) => return pages::not_found(site.look()),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "a static file link could not be had");
            return pages::unavailable(site.look());
        }
    };
    if entry.size > MAX_FILE_BYTES {
        tracing::warn!(target: "ruum::gateway", size = entry.size, "a static file is too large to serve");
        return pages::unavailable(site.look());
    }
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
    let Some(url) = link_url(&entry.url) else {
        tracing::warn!(target: "ruum::gateway", "the core answered a static file link that is not an HTTP URL");
        return pages::unavailable(site.look());
    };
    let resp = match gw.files_http.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e.without_url(), "a static file could not be fetched");
            return pages::unavailable(site.look());
        }
    };
    match resp.status() {
        s if s.is_success() => {}
        reqwest::StatusCode::NOT_FOUND => return pages::not_found(site.look()),
        s => {
            tracing::warn!(target: "ruum::gateway", status = %s, "a static file link was refused");
            return pages::unavailable(site.look());
        }
    }
    if resp.content_length().is_some_and(|n| n != entry.size) {
        tracing::warn!(target: "ruum::gateway", "a static file object does not have the size of its index entry");
        return pages::unavailable(site.look());
    }
    let limit = entry.size;
    let mut seen = 0u64;
    let stream = resp.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(|e| std::io::Error::other(e.without_url().to_string()))?;
        seen = seen.saturating_add(chunk.len() as u64);
        if seen > limit {
            return Err(std::io::Error::other(
                "a static file object is longer than its index entry",
            ));
        }
        Ok(chunk)
    });
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
    fn only_http_links_are_fetched() {
        assert!(
            link_url(
                "https://b.hel1.your-objectstorage.com/apps/static/t/files/x?X-Amz-Signature=1"
            )
            .is_some()
        );
        assert!(link_url("http://127.0.0.1:3900/ruum-dev-static/apps/static/t/files/x").is_some());
        for bad in [
            "file:///etc/passwd",
            "unix:/run/x",
            "ftp://h/x",
            "not a url",
        ] {
            assert!(link_url(bad).is_none(), "{bad}");
        }
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
