//! The node tree of one tenant as a `dav-server` file system.
//!
//! Every call resolves the path in the tenant of the request context and checks the permission
//! with [`authz::require`] before it reads or writes. Folders are collections and files are
//! files. Links and typed nodes are not shown in step 1.

use crate::error::fs_error;
use crate::file::{Reader, UploadLimits, WriteOpts, Writer, upload_limits};
use dav_server::davpath::DavPath;
use dav_server::fs::{
    DavDirEntry, DavFile, DavMetaData, FsError, FsFuture, FsResult, FsStream, GuardedFileSystem,
    OpenOptions, ReadDirMeta,
};
use futures::FutureExt;
use ruum_core::Services;
use ruum_core::actor::Context;
use ruum_core::authz::{self, Permission};
use ruum_core::blobs::{Hash, NewUpload, SMALL_CHUNKING, UploadTarget};
use ruum_core::error::ErrorCode;
use ruum_core::nodes::{ChildOrder, NewNode, Node, NodeKind, OnConflict};
use ruum_core::paging::PageRequest;
use std::path::PathBuf;
use std::time::SystemTime;

/// The credentials of one request: the context of the signed-in principal in its tenant.
#[derive(Clone)]
pub struct Creds {
    pub ctx: Context,
}

/// The file system over the core services.
#[derive(Clone)]
pub struct RuumFs {
    pub(crate) s: Services,
    /// Where PUT bodies are spooled (`None`: the system temporary folder).
    pub(crate) spool_dir: Option<PathBuf>,
}

/// The decoded path on the WebDAV host as a string. `/` is the tenant root.
pub(crate) fn path_str(path: &DavPath) -> FsResult<String> {
    String::from_utf8(path.as_bytes().to_vec()).map_err(|_| FsError::NotFound)
}

/// The names of a path on the WebDAV host, without empty segments.
pub(crate) fn names(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Whether WebDAV shows this node: folders and files only, in step 1.
pub(crate) fn shown(node: &Node) -> bool {
    matches!(node.kind, NodeKind::Folder | NodeKind::File) && node.trashed_at.is_none()
}

/// Resolves a path on the WebDAV host to a live node that the actor can see.
///
/// The root (`/`) is not secret: a caller who cannot see it (a guest, an agent scoped to a
/// subtree) gets `403`, as from `NodeStore::root`. Such callers mount the folder they can see
/// by its deep URL, for example `/Projects/Garden/`; there is no virtual root in step 1.
pub(crate) async fn resolve(s: &Services, ctx: &Context, path: &str) -> ruum_core::Result<Node> {
    let parts = names(path);
    let node = if parts.is_empty() {
        s.nodes.root(ctx).await?
    } else {
        s.nodes.get_by_path(ctx, &parts.join("/")).await?
    };
    if !shown(&node) {
        return Err(ruum_core::Error::not_found("node"));
    }
    match authz::require(&*s.authz, &*s.nodes, ctx, node.id, Permission::View).await {
        Err(ruum_core::Error::Api(e)) if parts.is_empty() && e.code == ErrorCode::NotFound => {
            Err(ruum_core::Error::permission_denied())
        }
        other => other.map(|()| node),
    }
}

/// Splits a path into the parent path and the last name. `None` for the root.
pub(crate) fn split_last(path: &str) -> Option<(String, String)> {
    let parts = names(path);
    let (last, parent) = parts.split_last()?;
    Some((parent.join("/"), (*last).to_string()))
}

/// Resolves the parent folder of a path, and returns it with the last name.
pub(crate) async fn resolve_parent(
    s: &Services,
    ctx: &Context,
    path: &str,
) -> ruum_core::Result<(Node, String)> {
    let Some((parent, name)) = split_last(path) else {
        return Err(ruum_core::Error::permission_denied());
    };
    let parent = resolve(s, ctx, &parent).await?;
    if parent.kind != NodeKind::Folder {
        return Err(ruum_core::Error::not_found("folder"));
    }
    Ok((parent, name))
}

/// The metadata of a node for `dav-server`.
#[derive(Clone, Debug)]
pub(crate) struct Meta {
    pub dir: bool,
    pub len: u64,
    pub modified: SystemTime,
    pub created: SystemTime,
    pub etag: String,
}

impl Meta {
    pub fn of(node: &Node) -> Self {
        Meta {
            dir: node.kind == NodeKind::Folder,
            len: if node.kind == NodeKind::File {
                node.size_bytes.max(0) as u64
            } else {
                0
            },
            modified: node.updated_at.into(),
            created: node.created_at.into(),
            etag: etag(node),
        }
    }
}

/// The entity tag of a node. For a file it is the head version ID and the node revision: saves
/// by the same actor within the coalescing window (design 5.3) change the content of the head
/// version without a new version ID, but every save grows the revision. For a folder (and a
/// file without content) the node ID and revision.
pub(crate) fn etag(node: &Node) -> String {
    match (node.kind, node.head_version) {
        (NodeKind::File, Some(v)) => format!("{v}-{}", node.revision),
        _ => format!("{}-{}", node.id, node.revision),
    }
}

impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        self.len
    }
    fn modified(&self) -> FsResult<SystemTime> {
        Ok(self.modified)
    }
    fn created(&self) -> FsResult<SystemTime> {
        Ok(self.created)
    }
    fn is_dir(&self) -> bool {
        self.dir
    }
    fn etag(&self) -> Option<String> {
        Some(self.etag.clone())
    }
}

struct Entry {
    name: String,
    meta: Meta,
}

impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.as_bytes().to_vec()
    }
    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let m: Box<dyn DavMetaData> = Box::new(self.meta.clone());
        async move { Ok(m) }.boxed()
    }
}

impl RuumFs {
    async fn node(&self, path: &DavPath, ctx: &Context) -> FsResult<Node> {
        let p = path_str(path)?;
        resolve(&self.s, ctx, &p).await.map_err(|e| fs_error(&e))
    }

    async fn children(&self, ctx: &Context, folder: &Node) -> FsResult<Vec<Node>> {
        let mut out = Vec::new();
        let mut page = PageRequest {
            cursor: None,
            limit: Some(ruum_core::paging::MAX_PAGE_SIZE),
        };
        loop {
            let p = self
                .s
                .nodes
                .list_children(ctx, folder.id, ChildOrder::Name, page)
                .await
                .map_err(|e| fs_error(&e))?;
            out.extend(p.items.into_iter().filter(shown));
            match p.next_cursor {
                Some(c) => {
                    page = PageRequest {
                        cursor: Some(c),
                        limit: Some(ruum_core::paging::MAX_PAGE_SIZE),
                    }
                }
                None => return Ok(out),
            }
        }
    }

    fn write_opts(&self, expected: Option<u64>, limits: &UploadLimits) -> WriteOpts {
        WriteOpts {
            expected,
            max: limits.max_file,
            spool_dir: self.spool_dir.clone(),
        }
    }

    async fn open_write(
        &self,
        path: &DavPath,
        options: OpenOptions,
        ctx: &Context,
    ) -> FsResult<Box<dyn DavFile>> {
        // Partial writes (Content-Range, PATCH) are not supported: every PUT is a whole version.
        // `write` + `create` without `truncate` is LOCK on a name (handled below).
        if options.append {
            return Err(FsError::NotImplemented);
        }
        let p = path_str(path)?;
        let (parent, name) = resolve_parent(&self.s, ctx, &p)
            .await
            .map_err(|e| fs_error(&e))?;
        let existing = match resolve(&self.s, ctx, &p).await {
            Ok(n) => Some(n),
            Err(ruum_core::Error::Api(e)) if e.code == ErrorCode::NotFound => None,
            Err(e) => return Err(fs_error(&e)),
        };
        let writer = match existing {
            Some(_) if options.create_new => return Err(FsError::Exists),
            Some(node) if node.kind != NodeKind::File => return Err(FsError::Forbidden),
            Some(_) if !options.truncate => return Err(FsError::NotImplemented),
            Some(node) => {
                authz::require(
                    &*self.s.authz,
                    &*self.s.nodes,
                    ctx,
                    node.id,
                    Permission::Edit,
                )
                .await
                .map_err(|e| fs_error(&e))?;
                let limits = upload_limits(&self.s, ctx).await;
                if let Some(size) = options.size {
                    limits.check(size, node.size_bytes.max(0) as u64)?;
                }
                let opts = self.write_opts(options.size, &limits);
                Writer::existing(self.s.clone(), ctx.clone(), node, opts)
            }
            None if !options.create => return Err(FsError::NotFound),
            None => {
                authz::require(
                    &*self.s.authz,
                    &*self.s.nodes,
                    ctx,
                    parent.id,
                    Permission::Edit,
                )
                .await
                .map_err(|e| fs_error(&e))?;
                if !options.truncate {
                    // LOCK on a name that does not exist yet (Windows, Office): make an empty
                    // file now, so the name is taken and the PUT that follows is a new version.
                    let node = self.create_empty(ctx, &parent, name).await?;
                    return Ok(Box::new(Writer::stored(self.s.clone(), ctx.clone(), node)));
                }
                let limits = upload_limits(&self.s, ctx).await;
                if let Some(size) = options.size {
                    limits.check(size, 0)?;
                }
                let opts = self.write_opts(options.size, &limits);
                Writer::new_file(self.s.clone(), ctx.clone(), parent, name, opts)
            }
        };
        Ok(Box::new(writer))
    }

    /// Makes an empty file through the upload flow (no chunks, the BLAKE3 hash of no bytes).
    async fn create_empty(&self, ctx: &Context, parent: &Node, name: String) -> FsResult<Node> {
        let t = &self.s.transfers;
        let content_type = crate::file::content_type(&name);
        let upload = t
            .create_upload(
                ctx,
                NewUpload {
                    target: UploadTarget::NewFile {
                        parent_id: parent.id,
                        name,
                        on_conflict: OnConflict::Fail,
                    },
                    content_type: Some(content_type),
                    chunking: SMALL_CHUNKING.id.to_string(),
                    size: 0,
                    content_hash: Hash(*blake3::hash(&[]).as_bytes()),
                    chunks: Vec::new(),
                },
            )
            .await
            .map_err(|e| fs_error(&e))?;
        match t.complete_upload(ctx, upload.id).await {
            Ok(w) => Ok(w.value.0),
            Err(e) => {
                if let Err(abort) = t.abort_upload(ctx, upload.id).await {
                    tracing::warn!(error = %abort, "webdav: cannot abort an upload");
                }
                Err(fs_error(&e))
            }
        }
    }
}

impl GuardedFileSystem<Creds> for RuumFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
        c: &'a Creds,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        async move {
            if options.write || options.append || options.create || options.create_new {
                return self.open_write(path, options, &c.ctx).await;
            }
            let node = self.node(path, &c.ctx).await?;
            if node.kind != NodeKind::File {
                return Err(FsError::Forbidden);
            }
            Ok(Box::new(Reader::new(self.s.clone(), c.ctx.clone(), node)) as Box<dyn DavFile>)
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
        c: &'a Creds,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        async move {
            let node = self.node(path, &c.ctx).await?;
            if node.kind != NodeKind::Folder {
                return Err(FsError::Forbidden);
            }
            let entries: Vec<FsResult<Box<dyn DavDirEntry>>> = self
                .children(&c.ctx, &node)
                .await?
                .into_iter()
                .map(|n| {
                    Ok(Box::new(Entry {
                        meta: Meta::of(&n),
                        name: n.name,
                    }) as Box<dyn DavDirEntry>)
                })
                .collect();
            Ok(Box::pin(futures::stream::iter(entries)) as FsStream<Box<dyn DavDirEntry>>)
        }
        .boxed()
    }

    fn metadata<'a>(
        &'a self,
        path: &'a DavPath,
        c: &'a Creds,
    ) -> FsFuture<'a, Box<dyn DavMetaData>> {
        async move {
            let node = self.node(path, &c.ctx).await?;
            Ok(Box::new(Meta::of(&node)) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath, c: &'a Creds) -> FsFuture<'a, ()> {
        async move {
            let p = path_str(path)?;
            let (parent, name) = resolve_parent(&self.s, &c.ctx, &p)
                .await
                .map_err(|e| fs_error(&e))?;
            authz::require(
                &*self.s.authz,
                &*self.s.nodes,
                &c.ctx,
                parent.id,
                Permission::Edit,
            )
            .await
            .map_err(|e| fs_error(&e))?;
            self.s
                .nodes
                .create(
                    &c.ctx,
                    NewNode {
                        parent_id: parent.id,
                        name,
                        kind: NodeKind::Folder,
                        node_type: None,
                        link: None,
                        on_conflict: OnConflict::Fail,
                    },
                )
                .await
                .map_err(|e| fs_error(&e))?;
            Ok(())
        }
        .boxed()
    }

    /// The quota properties (`quota-used-bytes`, `quota-available-bytes`), which Finder shows
    /// as the size of the volume: the tenant's storage use and its plan limit. Only for those
    /// who may read the usage (tenant members); for others the properties are left out.
    fn get_quota<'a>(&'a self, c: &'a Creds) -> FsFuture<'a, (u64, Option<u64>)> {
        async move {
            let usage = self
                .s
                .tenants
                .usage(&c.ctx)
                .await
                .map_err(|_| FsError::NotImplemented)?;
            let tenant = self
                .s
                .tenants
                .get(&c.ctx)
                .await
                .map_err(|_| FsError::NotImplemented)?;
            let limits = ruum_core::tenants::limits::tenant_limits(tenant.plan, usage.members);
            Ok((
                usage.storage_bytes.max(0) as u64,
                Some(limits.storage_bytes.max(0) as u64),
            ))
        }
        .boxed()
    }

    // DELETE, MOVE and COPY are handled in `crate::ops` before `dav-server` sees them, so a
    // folder is trashed, moved or copied as one node (one journal entry, one trash item).
}
