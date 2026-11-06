//! DELETE, MOVE and COPY. They map one to one to node operations, so a folder is trashed, moved
//! or copied as one node: one journal entry, one item in the trash, shared chunks for a copy.
//! (`dav-server` would walk the tree and change each node on its own.)

use crate::DAV_PATH;
use crate::error::{response, status};
use crate::file::{WriteOpts, copy_content, upload_limits};
use crate::fs::{resolve, resolve_parent};
use axum::body::Body;
use axum::http::{HeaderMap, Method, Response, StatusCode, Uri};
use dav_server::davpath::DavPath;
use dav_server::fs::FsError;
use ruum_core::Services;
use ruum_core::actor::Context;
use ruum_core::authz::{self, Permission};
use ruum_core::error::{Error, ErrorCode};
use ruum_core::nodes::{
    CopyRequest, MoveRequest, NewNode, Node, NodeKind, OnConflict, RestoreRequest,
};
use std::path::PathBuf;

/// Handles DELETE, MOVE or COPY on `uri`.
pub(crate) async fn handle(
    s: &Services,
    ctx: &Context,
    spool_dir: Option<PathBuf>,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Response<Body> {
    let Some(path) = dav_path(uri.path()) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let result = match method.as_str() {
        "DELETE" => delete(s, ctx, &path, headers).await,
        "MOVE" => copy_or_move(s, ctx, spool_dir, &path, headers, true).await,
        _ => copy_or_move(s, ctx, spool_dir, &path, headers, false).await,
    };
    result.unwrap_or_else(|e| response(&e))
}

/// The decoded path below `/dav` of a URL path, for example `/Projects/a b.txt`.
fn dav_path(url_path: &str) -> Option<String> {
    if url_path != DAV_PATH && !url_path.starts_with("/dav/") {
        return None;
    }
    let mut p = DavPath::new(url_path).ok()?;
    p.set_prefix(DAV_PATH).ok()?;
    String::from_utf8(p.as_bytes().to_vec()).ok()
}

/// The decoded path below `/dav` of a `Destination` header (an absolute URL or a path).
fn destination(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("destination")?.to_str().ok()?;
    let uri: Uri = raw.parse().ok()?;
    dav_path(uri.path())
}

fn is_root(path: &str) -> bool {
    crate::fs::names(path).is_empty()
}

fn not_found(e: &Error) -> bool {
    matches!(e, Error::Api(api) if api.code == ErrorCode::NotFound)
}

async fn require(s: &Services, ctx: &Context, node: &Node, p: Permission) -> ruum_core::Result<()> {
    authz::require(&*s.authz, &*s.nodes, ctx, node.id, p).await
}

/// DELETE: moves the node and its subtree to the trash. Needs `manage` (as `trashNode`).
async fn delete(
    s: &Services,
    ctx: &Context,
    path: &str,
    headers: &HeaderMap,
) -> ruum_core::Result<Response<Body>> {
    if is_root(path) {
        return Ok(status(StatusCode::FORBIDDEN));
    }
    let node = resolve(s, ctx, path).await?;
    if !if_match(headers, &node) {
        return Ok(status(StatusCode::PRECONDITION_FAILED));
    }
    require(s, ctx, &node, Permission::Manage).await?;
    s.nodes.trash(ctx, node.id, Some(node.revision)).await?;
    Ok(status(StatusCode::NO_CONTENT))
}

/// MOVE and COPY (RFC 4918, sections 9.8 and 9.9).
///
/// - MOVE in the same folder is a rename (`edit` on the node); to another folder it is a move
///   (`manage` on the node and `edit` on the new parent), with a new name if it changes.
/// - COPY needs `view` on the node and `edit` on the new parent. `Depth: 0` on a folder makes an
///   empty folder.
/// - A file onto an existing file (when `Overwrite` is not `F`): the content becomes a new head
///   version of the destination (`edit` on it), which keeps its node ID, versions, grants and
///   tags. MOVE then trashes the source (`manage` on it). This is how editors save: they write
///   a temporary file and move it over the original.
/// - Any other existing destination (a folder, or a file and a folder) is moved to the trash
///   first (`manage` on it), and restored if the operation then fails.
/// - With `Overwrite: F` an existing destination gives `412`.
async fn copy_or_move(
    s: &Services,
    ctx: &Context,
    spool_dir: Option<PathBuf>,
    path: &str,
    headers: &HeaderMap,
    is_move: bool,
) -> ruum_core::Result<Response<Body>> {
    let Some(dest) = destination(headers) else {
        return Ok(status(StatusCode::BAD_REQUEST));
    };
    let overwrite = headers
        .get("overwrite")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| !v.trim().eq_ignore_ascii_case("f"));
    let depth_zero = match headers.get("depth").and_then(|v| v.to_str().ok()) {
        None => false,
        Some(d) if d.trim().eq_ignore_ascii_case("infinity") => false,
        Some(d) if d.trim() == "0" && !is_move => true,
        Some(_) => return Ok(status(StatusCode::BAD_REQUEST)),
    };
    if is_root(path) && is_move {
        return Ok(status(StatusCode::FORBIDDEN));
    }
    if is_root(&dest) {
        return Ok(status(StatusCode::FORBIDDEN));
    }
    let node = resolve(s, ctx, path).await?;
    if !if_match(headers, &node) {
        return Ok(status(StatusCode::PRECONDITION_FAILED));
    }
    let (parent, name) = match resolve_parent(s, ctx, &dest).await {
        Ok(v) => v,
        // The parent of the destination does not exist (RFC 4918: 409).
        Err(e) if not_found(&e) => return Ok(status(StatusCode::CONFLICT)),
        Err(e) => return Err(e),
    };
    let existing = match resolve(s, ctx, &dest).await {
        Ok(n) => Some(n),
        Err(e) if not_found(&e) => None,
        Err(e) => return Err(e),
    };
    // Names are unique without regard to case, so `a.txt` -> `A.txt` finds the node itself.
    let existing = existing.filter(|n| n.id != node.id);
    if existing.is_some() && !overwrite {
        return Ok(status(StatusCode::PRECONDITION_FAILED));
    }
    // Not into itself or its own subtree (checked before anything changes).
    if node.id == parent.id || s.nodes.ancestors(ctx, parent.id).await?.contains(&node.id) {
        return Ok(status(StatusCode::FORBIDDEN));
    }

    if let Some(old) = existing
        .as_ref()
        .filter(|o| o.kind == NodeKind::File && node.kind == NodeKind::File)
    {
        return file_over_file(s, ctx, spool_dir, &node, old.clone(), is_move).await;
    }

    // Check every right and every precondition before the trash, so a refused request changes
    // nothing.
    check_rights(s, ctx, &node, &parent, is_move).await?;
    let mut trashed = None;
    if let Some(old) = &existing {
        require(s, ctx, old, Permission::Manage).await?;
        // The old destination must not hold the source (MOVE /a/b/c onto /a/b).
        if s.nodes.ancestors(ctx, node.id).await?.contains(&old.id) {
            return Ok(status(StatusCode::FORBIDDEN));
        }
        trashed = Some(s.nodes.trash(ctx, old.id, Some(old.revision)).await?.value);
    }
    let result = run(s, ctx, &node, &parent, name, is_move, depth_zero).await;
    if let Err(e) = result {
        if let Some(old) = trashed {
            // Put the old destination back, so a failed overwrite deletes nothing.
            let back = s
                .nodes
                .restore(
                    ctx,
                    old.id,
                    RestoreRequest {
                        parent_id: None,
                        on_conflict: OnConflict::Fail,
                        if_revision: Some(old.revision),
                    },
                )
                .await;
            if let Err(r) = back {
                tracing::error!(error = %r, node = %old.id, "webdav: cannot restore an overwritten node");
            }
        }
        return Err(e);
    }
    Ok(status(if existing.is_some() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::CREATED
    }))
}

/// The node operation of MOVE or COPY to `parent`/`name`, once the destination is free.
async fn run(
    s: &Services,
    ctx: &Context,
    node: &Node,
    parent: &Node,
    name: String,
    is_move: bool,
    depth_zero: bool,
) -> ruum_core::Result<()> {
    let new_name = (name != node.name).then_some(name.clone());
    if is_move {
        if node.parent_id == Some(parent.id) {
            if new_name.is_some() {
                s.nodes
                    .rename(ctx, node.id, name, OnConflict::Fail, Some(node.revision))
                    .await?;
            }
        } else {
            s.nodes
                .move_node(
                    ctx,
                    node.id,
                    MoveRequest {
                        parent_id: parent.id,
                        name: new_name,
                        on_conflict: OnConflict::Fail,
                        if_revision: Some(node.revision),
                    },
                )
                .await?;
        }
    } else if depth_zero && node.kind == NodeKind::Folder {
        s.nodes
            .create(
                ctx,
                NewNode {
                    parent_id: parent.id,
                    name,
                    kind: NodeKind::Folder,
                    node_type: None,
                    link: None,
                    on_conflict: OnConflict::Fail,
                },
            )
            .await?;
    } else {
        s.nodes
            .copy(
                ctx,
                node.id,
                CopyRequest {
                    parent_id: parent.id,
                    name: Some(name),
                    on_conflict: OnConflict::Fail,
                },
            )
            .await?;
    }
    Ok(())
}

/// MOVE or COPY of the file `node` onto the existing file `old`: the content of `node` becomes
/// a new head version of `old`; MOVE then trashes `node`.
async fn file_over_file(
    s: &Services,
    ctx: &Context,
    spool_dir: Option<PathBuf>,
    node: &Node,
    old: Node,
    is_move: bool,
) -> ruum_core::Result<Response<Body>> {
    // `view` on the source is checked by `resolve`.
    require(s, ctx, &old, Permission::Edit).await?;
    if is_move {
        require(s, ctx, node, Permission::Manage).await?;
    }
    let limits = upload_limits(s, ctx).await;
    let size = node.size_bytes.max(0) as u64;
    let opts = WriteOpts {
        expected: Some(size),
        max: limits.max_file,
        spool_dir,
    };
    let checked = match limits.check(size, old.size_bytes.max(0) as u64) {
        Ok(()) => copy_content(s, ctx, node, old, opts).await,
        Err(e) => Err(e),
    };
    if let Err(e) = checked {
        return Ok(status(fs_status(e)));
    }
    if is_move {
        s.nodes.trash(ctx, node.id, Some(node.revision)).await?;
    }
    Ok(status(StatusCode::NO_CONTENT))
}

/// The HTTP status of a file system error (as `dav-server` answers it).
fn fs_status(e: FsError) -> StatusCode {
    match e {
        FsError::NotFound => StatusCode::NOT_FOUND,
        FsError::Forbidden => StatusCode::FORBIDDEN,
        FsError::Exists => StatusCode::CONFLICT,
        FsError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        FsError::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        FsError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn check_rights(
    s: &Services,
    ctx: &Context,
    node: &Node,
    parent: &Node,
    is_move: bool,
) -> ruum_core::Result<()> {
    if !is_move {
        return require(s, ctx, parent, Permission::Edit).await;
    }
    if node.parent_id == Some(parent.id) {
        require(s, ctx, node, Permission::Edit).await
    } else {
        require(s, ctx, node, Permission::Manage).await?;
        require(s, ctx, parent, Permission::Edit).await
    }
}

/// The `If-Match` precondition against the node's entity tag (`*` matches any node).
fn if_match(headers: &HeaderMap, node: &Node) -> bool {
    let Some(v) = headers.get("if-match").and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let tag = crate::fs::etag(node);
    v.split(',').any(|t| {
        let t = t.trim();
        t == "*" || t.trim_start_matches("W/").trim_matches('"') == tag
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_paths_and_destinations() {
        assert_eq!(dav_path("/dav").as_deref(), Some("/"));
        assert_eq!(dav_path("/dav/").as_deref(), Some("/"));
        assert_eq!(dav_path("/dav/a%20b/c.txt").as_deref(), Some("/a b/c.txt"));
        assert_eq!(dav_path("/other/x"), None);
        assert_eq!(dav_path("/davx/y"), None);
        let mut h = HeaderMap::new();
        h.insert(
            "destination",
            "https://ruum.localhost/dav/New%20Folder/x.txt"
                .parse()
                .unwrap(),
        );
        assert_eq!(destination(&h).as_deref(), Some("/New Folder/x.txt"));
        h.insert("destination", "/dav/y".parse().unwrap());
        assert_eq!(destination(&h).as_deref(), Some("/y"));
        h.insert("destination", "/elsewhere/y".parse().unwrap());
        assert_eq!(destination(&h), None);
    }
}
