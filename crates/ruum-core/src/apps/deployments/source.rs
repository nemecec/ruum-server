//! Reading the source of a deployment: a file node version (an archive) or a folder that the
//! platform packs (decision 0006-2, review RC-1).
//!
//! A source the caller cannot view, outside an agent's scope or in another tenant fails exactly
//! like a missing node ([`source_not_found`]); a version that is not a version of the file
//! fails exactly like a missing version ([`version_not_found`]). Both errors carry no name and
//! no ID.

use super::super::AppService;
use super::super::archive::{self, Limits};
use super::super::store::{DeploymentSource, SourceFile};
use super::{Packed, SourceRequest};
use crate::actor::Context;
use crate::authz::{self, Role};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::NodeId;
use crate::nodes::{ChildOrder, Node, NodeKind};
use crate::paging::PageRequest;
use futures::StreamExt;

/// The deepest folder nesting a folder source packs.
const MAX_FOLDER_DEPTH: usize = 32;

/// The answer for a source that the caller cannot use: the same for a missing node, a node of
/// another tenant, a node outside an agent's scope and a node the caller cannot view.
pub fn source_not_found() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::NotFound, "the source node was not found")
            .with_hint("check source.node_id; the caller needs view on the source"),
    )
}

/// The answer for a version that is not a version of the source file.
pub fn version_not_found() -> Error {
    Error::Api(
        ApiError::new(ErrorCode::NotFound, "the source version was not found")
            .with_hint("leave out source.version_id to deploy the head version"),
    )
}

fn too_large(what: &'static str, limit: u64) -> Error {
    archive::Refusal::Limit { what, limit }.into_error()
}

fn wrong_kind(kind: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::ValidationFailed,
            format!("the source is not a {kind}"),
        )
        .with_hint("/source/kind")
        .with_details(serde_json::json!({ "pointer": "/source/kind" })),
    )
}

impl AppService {
    /// The source node, with `view` checked; every refusal is [`source_not_found`].
    async fn source_node(&self, ctx: &Context, id: NodeId) -> Result<Node> {
        let d = &self.deps;
        let node = d.nodes.get(ctx, id).await.map_err(|e| match e {
            Error::Api(_) => source_not_found(),
            other => other,
        })?;
        authz::require(&*d.authz, &*d.nodes, ctx, id, Role::View)
            .await
            .map_err(|e| match e {
                Error::Api(_) => source_not_found(),
                other => other,
            })?;
        if node.trashed_at.is_some() {
            return Err(Error::Api(
                ApiError::new(ErrorCode::Conflict, "the source is in the trash")
                    .with_hint("restore it first (restoreNode)"),
            ));
        }
        Ok(node)
    }

    /// Reads one file version into memory, at most `max` bytes. `None` for `version` is the
    /// head version. Returns the bytes and the version that was read.
    async fn read_file(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<crate::ids::VersionId>,
        max: u64,
    ) -> Result<(Vec<u8>, crate::ids::VersionId)> {
        let content = self
            .deps
            .transfers
            .open_content(ctx, node, version, None)
            .await
            .map_err(|e| match e {
                Error::Api(_) if version.is_some() => version_not_found(),
                Error::Api(_) => source_not_found(),
                other => other,
            })?;
        if content.total_size > max {
            return Err(too_large("archive bytes", max));
        }
        let mut out = Vec::with_capacity(content.total_size as usize);
        let mut body = content.body;
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            if (out.len() + chunk.len()) as u64 > max {
                return Err(too_large("archive bytes", max));
            }
            out.extend_from_slice(&chunk);
        }
        Ok((out, content.version_id))
    }

    /// The archive bytes and the recorded source of a request.
    pub(crate) async fn read_source(
        &self,
        ctx: &Context,
        req: &SourceRequest,
        limits: &Limits,
    ) -> Result<Packed> {
        let node = self.source_node(ctx, req.node_id).await?;
        match req.source_kind() {
            "file" => {
                if node.kind != NodeKind::File {
                    return Err(wrong_kind("file (an archive)"));
                }
                let (bytes, version) = self
                    .read_file(ctx, node.id, req.version_id, limits.max_total_bytes)
                    .await?;
                Ok(Packed {
                    source: DeploymentSource {
                        kind: "file".into(),
                        node_id: Some(node.id),
                        version_id: Some(version),
                        files: None,
                    },
                    bytes,
                })
            }
            _ => {
                if node.kind != NodeKind::Folder {
                    return Err(wrong_kind("folder"));
                }
                self.pack_folder(ctx, &node, limits).await
            }
        }
    }

    /// Packs the head versions of the files of a folder (and its subfolders) into a ustar
    /// archive, under the limits of the plan.
    async fn pack_folder(&self, ctx: &Context, folder: &Node, limits: &Limits) -> Result<Packed> {
        // Collect the files first (names, nodes, head versions), then read them.
        let mut found: Vec<(String, Node)> = Vec::new();
        let mut stack: Vec<(NodeId, String, usize)> = vec![(folder.id, String::new(), 0)];
        let mut declared: u64 = 0;
        while let Some((id, prefix, depth)) = stack.pop() {
            let mut cursor = None;
            loop {
                let page = self
                    .deps
                    .nodes
                    .list_children(
                        ctx,
                        id,
                        ChildOrder::Name,
                        PageRequest {
                            cursor,
                            limit: Some(200),
                        },
                    )
                    .await?;
                for child in page.items {
                    if child.trashed_at.is_some() {
                        continue;
                    }
                    let path = format!("{prefix}{}", child.name);
                    match child.kind {
                        NodeKind::Folder => {
                            if depth + 1 > MAX_FOLDER_DEPTH {
                                return Err(Error::Api(
                                    ApiError::new(
                                        ErrorCode::ValidationFailed,
                                        "the folder nests too deep",
                                    )
                                    .with_hint(format!(
                                        "keep the site within {MAX_FOLDER_DEPTH} folder levels"
                                    )),
                                ));
                            }
                            stack.push((child.id, format!("{path}/"), depth + 1));
                        }
                        NodeKind::File if child.head_version.is_some() => {
                            declared = declared.saturating_add(child.size_bytes.max(0) as u64);
                            if declared > limits.max_total_bytes {
                                return Err(too_large("unpacked bytes", limits.max_total_bytes));
                            }
                            found.push((path, child));
                            if found.len() as u64 > u64::from(limits.max_files) {
                                return Err(too_large("files", u64::from(limits.max_files)));
                            }
                        }
                        // Links, typed nodes and files without content are not packed.
                        _ => {}
                    }
                }
                match page.next_cursor {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        let mut contents: Vec<(String, Vec<u8>)> = Vec::with_capacity(found.len());
        let mut files = Vec::with_capacity(found.len());
        let mut total: u64 = 0;
        for (path, node) in &found {
            let left = limits.max_total_bytes.saturating_sub(total);
            let (bytes, version) = self
                .read_file(
                    ctx,
                    node.id,
                    node.head_version,
                    left.min(limits.max_entry_bytes),
                )
                .await
                .map_err(|e| match e {
                    // A file that went away between the listing and the read.
                    Error::Api(a) if a.code == ErrorCode::NotFound => Error::Api(
                        ApiError::new(
                            ErrorCode::Conflict,
                            "a file of the folder changed while it was packed",
                        )
                        .with_hint("deploy again"),
                    ),
                    other => other,
                })?;
            total += bytes.len() as u64;
            files.push(SourceFile {
                node_id: node.id,
                version_id: version,
                path: path.clone(),
            });
            contents.push((path.clone(), bytes));
        }
        let refs: Vec<(String, &[u8])> = contents
            .iter()
            .map(|(p, b)| (p.clone(), b.as_slice()))
            .collect();
        let bytes = archive::pack_tar(&refs)?;
        Ok(Packed {
            source: DeploymentSource {
                kind: "folder".into(),
                node_id: Some(folder.id),
                version_id: None,
                files: Some(files),
            },
            bytes,
        })
    }
}

impl SourceRequest {
    fn source_kind(&self) -> &str {
        &self.kind
    }
}
