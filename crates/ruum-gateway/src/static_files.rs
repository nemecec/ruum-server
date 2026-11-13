//! Static files from the object store (plan seam 1, `ruum_core::apps::static_site`; decision 0006
//! G point 4; backlog S-38).
//!
//! The gateway never builds an object key from the request path. It refuses a bad path first
//! (`request_path`: an empty segment, `.`, `..`, a `%`, a `\`, a control byte: `400`), then looks
//! the path up in the deployment's site index (an exact match, then `<path>/index.html`, then
//! `spa_fallback` for a navigation); a miss is its own `404`. The file streams from its
//! content-addressed object with a size limit, never read whole. `ETag` is the file's hash and
//! `Content-Type` comes from the index.

use crate::{Gateway, Site, pages};
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use futures::StreamExt;
use ruum_core::apps::static_site::request_path;

/// The largest static file the gateway serves (the plan's file size limit is smaller).
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

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
    let tenant = site.resolved.tenant_id;
    let index = match gw
        .sites
        .read_index(tenant, site.resolved.deployment_id)
        .await
    {
        Ok(Some(i)) => i,
        Ok(None) => return pages::not_found(site.look()),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "a site index could not be read");
            return pages::unavailable(site.look());
        }
    };
    let Some((_, entry)) = index.resolve(path, is_navigation(method, headers)) else {
        return pages::not_found(site.look());
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
    let stream = match gw.sites.open_file(tenant, entry, MAX_FILE_BYTES).await {
        Ok(s) => s,
        Err(ruum_core::Error::Api(_)) => return pages::not_found(site.look()),
        Err(e) => {
            tracing::warn!(target: "ruum::gateway", error = %e, "a static file could not be opened");
            return pages::unavailable(site.look());
        }
    };
    *r.body_mut() =
        Body::from_stream(stream.map(|c| c.map_err(|e| std::io::Error::other(e.to_string()))));
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
