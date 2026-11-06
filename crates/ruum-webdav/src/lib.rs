//! WebDAV face of the node tree (decision 12), built on `dav-server`. It maps WebDAV paths to
//! nodes of one tenant and uses the same core services as the REST API, so permissions, versions
//! and the journal work the same way. It runs on its own host (`https://dav.<domain>/`, its
//! own listener `RUUM_DAV_BIND`) at the root path `/`, never on the platform origin (decision
//! 0002-3): this router has only WebDAV routes, and the platform router has none.
//!
//! - Sign-in ([`auth`]): HTTP Basic with the user's email and an app password (actor = the
//!   user), or a Bearer agent access token (actor = the agent, on behalf of its owner, within
//!   its scope). Both are bound to one tenant; `/` is the root folder of that tenant. Cookies
//!   are never read and never set, so the session of the web UI never reaches this host.
//! - Every operation checks the permission with `authz::require` before it acts: `view` to read
//!   and list, `edit` on the parent to create (PUT, MKCOL, the destination of COPY and MOVE),
//!   `edit` on a file to write a new version, `edit` to rename, `manage` to move to another
//!   folder and to delete (DELETE moves to the trash, as `trashNode`).
//! - PROPFIND, GET and HEAD (with ranges), PUT (each PUT is one new version through the upload
//!   flow), MKCOL, LOCK and UNLOCK go through `dav-server`; DELETE, MOVE and COPY are mapped to
//!   one node operation each ([`ops`]).
//! - Locks are fake (`FakeLs`): the server answers LOCK so macOS Finder and Windows mount the
//!   share read-write, but it does not keep locks. Versions protect against lost work. LOCK on
//!   a name that does not exist yet makes an empty file (RFC 4918, section 9.10.4).
//! - MOVE or COPY of a file onto an existing file (the safe-save of Office, LibreOffice, vim and
//!   macOS) writes the content as a new version of the destination, so it keeps its node ID,
//!   versions, grants and tags; MOVE then trashes the source.
//! - PUT bodies are spooled to a temporary file (in `RUUM_DAV_SPOOL_DIR`, else the system
//!   temporary folder) and limited to the plan's `max_file_bytes` while they are written; a
//!   known size over that limit or over the free storage is refused before the body is read.
//! - The root `/` is the tenant root folder. Guests and agents scoped to a subtree cannot
//!   see it (`403`); they mount the folder they can see by its URL, for example
//!   `/Projects/Garden/`.
//! - User content never runs as a page (design section 15). The WebDAV host is apart from the
//!   platform origin, and as defence in depth every answer carries the content CSP (`sandbox`)
//!   and `X-Content-Type-Options: nosniff`, and `GET` and `HEAD` of a file also carry
//!   `Content-Disposition: attachment` and send active types (HTML, SVG, XML, scripts) as
//!   `application/octet-stream` ([`content_safety`]). So no script runs on the sibling host
//!   (it could otherwise set cookies for the parent domain).
//! - Not supported in step 1: partial PUT (`Content-Range`) and PATCH, PROPPATCH of dead
//!   properties, links and typed nodes (they are not listed).

mod auth;
mod error;
mod file;
mod fs;
mod ops;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use axum::routing::any;
use dav_server::fakels::FakeLs;
use dav_server::{DavHandler, DavMethod, DavMethodSet};
use ruum_core::Services;
use ruum_core::blobs::Disposition;
use ruum_core::blobs::signed::{CONTENT_CSP, disposition_header, passive_content_type};
use std::path::PathBuf;
use std::sync::Arc;

pub use auth::REALM;
pub use fs::{Creds, RuumFs};

/// The environment variable of the spool folder of PUT bodies.
pub const SPOOL_DIR_ENV: &str = "RUUM_DAV_SPOOL_DIR";

/// The options of the WebDAV endpoint.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Where PUT bodies are spooled before they are stored. `None`: the system temporary folder.
    pub spool_dir: Option<PathBuf>,
}

impl Options {
    /// The options from the environment: [`SPOOL_DIR_ENV`].
    pub fn from_env() -> Self {
        Options {
            spool_dir: std::env::var_os(SPOOL_DIR_ENV)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }
}

struct DavState {
    services: Services,
    dav: DavHandler<Creds>,
    spool_dir: Option<PathBuf>,
}

/// The WebDAV router, for the listener of the WebDAV host, with the options from the
/// environment. It serves every path from `/`; do not merge it into the platform router.
pub fn router(services: Services) -> Router {
    router_with(services, Options::from_env())
}

/// The WebDAV router with these options.
pub fn router_with(services: Services, options: Options) -> Router {
    let mut methods = DavMethodSet::WEBDAV_RW;
    // Partial updates and dead properties are not supported (see the module comment).
    methods.remove(DavMethod::Patch);
    methods.remove(DavMethod::PropPatch);
    let dav = DavHandler::<Creds>::builder()
        .filesystem(Box::new(RuumFs {
            s: services.clone(),
            spool_dir: options.spool_dir.clone(),
        }))
        .locksystem(FakeLs::new())
        .methods(methods)
        .autoindex(false)
        .build_handler();
    let state = Arc::new(DavState {
        services,
        dav,
        spool_dir: options.spool_dir,
    });
    Router::new()
        .route("/", any(handle))
        .route("/{*path}", any(handle))
        .with_state(state)
}

async fn handle(State(st): State<Arc<DavState>>, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    // Each request reads at least as fresh as its tenant's newest relationship write
    // (decision 0002-4).
    let mut res = ruum_core::authz::with_request_freshness(dispatch(&st, req)).await;
    content_safety(&method, &path, &mut res);
    // This host never sets cookies (decision 0002-3).
    res.headers_mut().remove(header::SET_COOKIE);
    res
}

/// Makes an answer safe to open in a browser (design section 15), as defence in depth next to
/// the separate host: the
/// content CSP (`sandbox`) and `nosniff` on every answer; on a successful `GET` or `HEAD` also
/// `Content-Disposition: attachment` with the file name, and a passive content type
/// (`blobs::signed::passive_content_type`).
fn content_safety(method: &Method, path: &str, res: &mut Response) {
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if (method != Method::GET && method != Method::HEAD)
        || !(res.status().is_success() || res.status() == StatusCode::NOT_MODIFIED)
    {
        return;
    }
    let h = res.headers_mut();
    let ct = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let safe = passive_content_type(&ct);
    if safe != ct
        && let Ok(v) = HeaderValue::from_str(safe)
    {
        h.insert(header::CONTENT_TYPE, v);
    }
    let name = ops::dav_path(path)
        .and_then(|p| fs::names(&p).last().map(|n| n.to_string()))
        .unwrap_or_else(|| "download".to_string());
    if let Ok(v) = HeaderValue::from_str(&disposition_header(Disposition::Attachment, &name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
}

async fn dispatch(st: &DavState, req: Request) -> Response {
    let ctx = match auth::context(&st.services, req.headers()).await {
        Ok(ctx) => ctx,
        Err(res) => return *res,
    };
    match req.method().as_str() {
        "DELETE" | "MOVE" | "COPY" => {
            let spool_dir = st.spool_dir.clone();
            ops::handle(
                &st.services,
                &ctx,
                spool_dir,
                req.method(),
                req.uri(),
                req.headers(),
            )
            .await
        }
        _ => {
            let principal = ctx.actor.actor.id.0.to_string();
            st.dav
                .handle_guarded(req, principal, Creds { ctx })
                .await
                .map(Body::new)
        }
    }
}
