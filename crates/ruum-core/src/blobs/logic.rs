//! Rules and helpers that the in-memory and the Postgres content stores share: request checks,
//! errors, content streams, content checks and the data of the journal entries.

use super::{
    BlobStore, ByteRange, ChunkRef, ChunkingParams, Hash, MAX_CHUNK_BYTES, Manifest, NewUpload,
    chunking_by_id, chunking_for, manifest_of,
};
use crate::actor::{Context, PrincipalKind};
use crate::authz::Role;
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{TenantId, VersionId};
use crate::tenants::limits::Limits;
use crate::versions::{MAX_LABEL_CHARS, Version};
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::{self, BoxStream};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

pub fn api(code: ErrorCode, message: impl Into<String>, hint: impl Into<String>) -> Error {
    Error::Api(ApiError::new(code, message).with_hint(hint))
}

pub fn validation(message: impl Into<String>, hint: impl Into<String>) -> Error {
    api(ErrorCode::ValidationFailed, message, hint)
}

pub fn upload_not_found() -> Error {
    api(
        ErrorCode::NotFound,
        "upload not found",
        "only the actor that started an upload can see it; start a new one with POST /v1/uploads",
    )
}

pub fn upload_not_open(status: super::UploadStatus) -> Error {
    api(
        ErrorCode::Conflict,
        format!(
            "the upload is {}",
            serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default()
        ),
        "start a new upload with POST /v1/uploads",
    )
}

pub fn chunk_missing(n: usize) -> Error {
    api(
        ErrorCode::ChunkMissing,
        format!("{n} chunk(s) of the upload are not stored yet"),
        format!(
            "upload the {n} chunk(s) in `missing` (GET /v1/uploads/{{upload_id}}), then complete again"
        ),
    )
}

pub fn version_mismatch() -> Error {
    api(
        ErrorCode::VersionMismatch,
        "the head version is no longer `base_version`",
        "read the node again, merge the changes, and upload again with the new head as base_version",
    )
}

pub fn precondition_failed() -> Error {
    api(
        ErrorCode::PreconditionFailed,
        "the head version is not the one in If-Match",
        "read the node again (head_version), merge the changes, and retry with the new version",
    )
}

pub fn version_not_found() -> Error {
    Error::not_found("version")
}

pub fn not_a_file() -> Error {
    validation(
        "the node is not a file",
        "content and versions exist only for files",
    )
}

pub fn not_a_folder() -> Error {
    validation(
        "the parent is not a folder",
        "a new file goes into a folder; pass a folder ID as parent_id",
    )
}

pub fn payload_too_large(max: i64) -> Error {
    api(
        ErrorCode::PayloadTooLarge,
        format!("the file is larger than {max} bytes, the largest file of the plan"),
        "upload a smaller file, or ask the tenant owner about a larger plan",
    )
}

pub fn quota_exceeded() -> Error {
    api(
        ErrorCode::QuotaExceeded,
        "the tenant has no storage left for this file",
        "delete files (and empty the trash), or ask the tenant owner about a larger plan",
    )
}

pub fn range_not_satisfiable(total: u64) -> Error {
    api(
        ErrorCode::RangeNotSatisfiable,
        format!("the range is outside the content ({total} bytes)"),
        "ask for a range inside the content size, or for the whole content",
    )
}

/// Share-link bearers write only through an `edit` link (contract, "Users only"); a `view` or
/// `comment` link gets `user_required`.
pub fn check_link_may_write(ctx: &Context) -> Result<()> {
    if ctx.actor.actor.kind == PrincipalKind::ShareLink
        && ctx.scope.as_ref().is_none_or(|s| s.max_role < Role::Edit)
    {
        return Err(Error::user_required());
    }
    Ok(())
}

/// Checks a node name before an upload starts: not empty, at most 255 characters, no `/`, no
/// NUL, not `.` or `..`, no spaces at the ends. The store of the node normalises it (NFC) and
/// checks that it is free when the file is made.
pub fn check_file_name(name: &str) -> Result<()> {
    let bad = |why: &str| {
        validation(
            format!("the name is not valid: {why}"),
            "use 1 to 255 characters, without `/`, and not `.` or `..`",
        )
    };
    if name.is_empty() {
        return Err(bad("empty"));
    }
    if name.chars().count() > crate::nodes::MAX_NAME_CHARS {
        return Err(bad("too long"));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(bad("it contains `/` or NUL"));
    }
    if name == "." || name == ".." {
        return Err(bad("`.` and `..` are reserved"));
    }
    if name.trim() != name {
        return Err(bad("spaces at the start or the end"));
    }
    Ok(())
}

/// Checks a new upload and returns its chunking parameters: a published `chunking`, chunk sizes
/// of 1 byte to [`MAX_CHUNK_BYTES`] (and at most the `max_size` of the set), sizes that add up
/// to `size`, and the hash of the empty content for an empty file.
pub fn check_new_upload(req: &NewUpload) -> Result<ChunkingParams> {
    let params = chunking_by_id(&req.chunking).ok_or_else(|| {
        validation(
            format!("the chunking `{}` is not published", req.chunking),
            "get the parameters from GET /v1/uploads/params and cut the file with them",
        )
    })?;
    let max = params.max_size.min(MAX_CHUNK_BYTES);
    let mut total: u64 = 0;
    for c in &req.chunks {
        if c.size == 0 || c.size > max {
            return Err(validation(
                format!("a chunk has {} bytes; chunks have 1 to {max} bytes", c.size),
                "cut the file with the published chunking parameters",
            ));
        }
        total += u64::from(c.size);
    }
    if total != req.size {
        return Err(validation(
            format!(
                "the chunk sizes add up to {total} bytes, but `size` is {}",
                req.size
            ),
            "send every chunk of the file in order",
        ));
    }
    if req.chunks.is_empty() && req.content_hash != Hash::of(b"") {
        return Err(validation(
            "an empty file must have the BLAKE3 hash of no bytes",
            "content_hash of an empty file is af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        ));
    }
    Ok(params)
}

/// Checks the limits of the plan for content of `new_size` bytes that replaces `old_size` bytes,
/// when the tenant uses `used` bytes now.
pub fn check_limits(limits: &Limits, used: i64, old_size: i64, new_size: u64) -> Result<()> {
    let new_size = i64::try_from(new_size).unwrap_or(i64::MAX);
    if new_size > limits.max_file_bytes {
        return Err(payload_too_large(limits.max_file_bytes));
    }
    if used.saturating_sub(old_size).saturating_add(new_size) > limits.storage_bytes {
        return Err(quota_exceeded());
    }
    Ok(())
}

/// The distinct hashes of the chunks, in order of first use.
pub fn distinct(chunks: &[ChunkRef]) -> Vec<Hash> {
    let mut seen = HashSet::new();
    chunks
        .iter()
        .map(|c| c.hash)
        .filter(|h| seen.insert(*h))
        .collect()
}

/// Checks a label: at most [`MAX_LABEL_CHARS`] characters, not empty. Returns it trimmed.
pub fn check_label(label: Option<String>) -> Result<Option<String>> {
    match label {
        None => Ok(None),
        Some(l) => {
            let l = l.trim().to_string();
            if l.is_empty() || l.chars().count() > MAX_LABEL_CHARS {
                Err(validation(
                    format!("a label has 1 to {MAX_LABEL_CHARS} characters"),
                    "send a shorter label, or null to remove it",
                ))
            } else {
                Ok(Some(l))
            }
        }
    }
}

/// Resolves a range against the content size: a start past the end fails with
/// `range_not_satisfiable`; an end past the end is cut to the last byte. A range over the whole
/// content is `None`.
pub fn resolve_range(range: Option<ByteRange>, total: u64) -> Result<Option<ByteRange>> {
    let Some(r) = range else {
        return Ok(None);
    };
    if r.start > r.end || r.start >= total {
        return Err(range_not_satisfiable(total));
    }
    let end = r.end.min(total - 1);
    if r.start == 0 && end == total - 1 {
        return Ok(None);
    }
    Ok(Some(ByteRange {
        start: r.start,
        end,
    }))
}

/// The bytes of the chunks in the range, as a stream that reads one chunk at a time.
pub fn stream_chunks(
    blobs: Arc<dyn BlobStore>,
    tenant: TenantId,
    chunks: Vec<ChunkRef>,
    range: Option<ByteRange>,
) -> BoxStream<'static, Result<Bytes>> {
    let total: u64 = chunks.iter().map(|c| u64::from(c.size)).sum();
    let (start, end) = match range {
        Some(r) => (r.start, r.end + 1),
        None => (0, total),
    };
    let mut offset = 0u64;
    let mut parts = Vec::new();
    for c in chunks {
        let (a, b) = (offset, offset + u64::from(c.size));
        offset = b;
        if b <= start || a >= end {
            continue;
        }
        let from = start.saturating_sub(a) as usize;
        let to = (end.min(b) - a) as usize;
        parts.push((c, from, to));
    }
    stream::iter(parts)
        .then(move |(c, from, to)| {
            let blobs = blobs.clone();
            async move {
                let data = blobs.get_chunk(tenant, c.hash).await?;
                if data.len() != c.size as usize {
                    return Err(Error::Internal(format!(
                        "chunk {} has {} bytes, the manifest says {}",
                        c.hash.to_hex(),
                        data.len(),
                        c.size
                    )));
                }
                Ok(data.slice(from..to))
            }
        })
        .boxed()
}

/// Reads all chunks of the manifest and checks each chunk size and the hash of the whole
/// content. Chunks that are gone give `chunk_missing` (with the list, for the caller to clean
/// up its chunk records); another content hash gives `hash_mismatch`.
pub async fn verify_content(
    blobs: &dyn BlobStore,
    tenant: TenantId,
    manifest: &Manifest,
) -> std::result::Result<(), (Error, Vec<Hash>)> {
    let present = blobs
        .has_chunks(tenant, &distinct(&manifest.chunks))
        .await
        .map_err(|e| (e, vec![]))?;
    let missing: Vec<Hash> = distinct(&manifest.chunks)
        .into_iter()
        .zip(present)
        .filter(|(_, p)| !p)
        .map(|(h, _)| h)
        .collect();
    if !missing.is_empty() {
        return Err((chunk_missing(missing.len()), missing));
    }
    let mut hasher = blake3::Hasher::new();
    for c in &manifest.chunks {
        let data = match blobs.get_chunk(tenant, c.hash).await {
            Ok(d) => d,
            Err(Error::Api(e)) if e.code == ErrorCode::NotFound => {
                return Err((chunk_missing(1), vec![c.hash]));
            }
            Err(e) => return Err((e, vec![])),
        };
        if data.len() != c.size as usize || Hash::of(&data) != c.hash {
            return Err((
                api(
                    ErrorCode::HashMismatch,
                    format!("the stored chunk {} does not match", c.hash.to_hex()),
                    "upload the chunk again",
                ),
                vec![c.hash],
            ));
        }
        hasher.update(&data);
    }
    if Hash(*hasher.finalize().as_bytes()) != manifest.content_hash {
        return Err((
            api(
                ErrorCode::HashMismatch,
                "the content does not have the hash `content_hash`",
                "hash the whole file with BLAKE3 and start a new upload",
            ),
            vec![],
        ));
    }
    Ok(())
}

/// Checks one uploaded chunk: it is in the upload's list and has that hash and size.
pub fn check_chunk(chunks: &[ChunkRef], hash: Hash, data: &[u8]) -> Result<()> {
    let Some(c) = chunks.iter().find(|c| c.hash == hash) else {
        return Err(validation(
            "the chunk is not in the upload's list",
            "upload only the hashes in `missing`",
        ));
    };
    if Hash::of(data) != hash {
        return Err(api(
            ErrorCode::HashMismatch,
            "the chunk does not have the hash in the URL",
            "hash the chunk with BLAKE3 and send it again",
        ));
    }
    if data.len() != c.size as usize {
        return Err(validation(
            format!(
                "the chunk has {} bytes, the upload says {}",
                data.len(),
                c.size
            ),
            "send the chunk as it was cut",
        ));
    }
    Ok(())
}

/// Chunks `data` for `putContent` and stores the chunks that the tenant does not have. Returns
/// the manifest and the stored chunks (hash, size) for the chunk records.
pub async fn store_data(
    blobs: &dyn BlobStore,
    tenant: TenantId,
    data: &Bytes,
    content_type: Option<&str>,
) -> Result<(Manifest, Vec<ChunkRef>)> {
    let params = chunking_for(data.len() as u64, content_type);
    let manifest = manifest_of(data, params);
    let hashes = distinct(&manifest.chunks);
    let have = blobs.has_chunks(tenant, &hashes).await?;
    let mut offset = 0usize;
    let mut stored = Vec::new();
    let mut done = HashSet::new();
    for c in &manifest.chunks {
        let (a, b) = (offset, offset + c.size as usize);
        offset = b;
        let i = hashes.iter().position(|h| *h == c.hash).unwrap_or(0);
        if have[i] || !done.insert(c.hash) {
            continue;
        }
        blobs.put_chunk(tenant, c.hash, data.slice(a..b)).await?;
        stored.push(*c);
    }
    Ok((manifest, stored))
}

/// The data of `version.created`.
pub fn version_created_data(v: &Version, previous: Option<VersionId>, coalesced: bool) -> Value {
    json!({
        "version_id": v.id,
        "previous_version_id": previous,
        "size_bytes": v.size_bytes,
        "content_hash": v.content_hash,
        "content_type": v.content_type,
        "coalesced": coalesced,
    })
}

/// The data of `version.restored`.
pub fn version_restored_data(v: &Version, previous: Option<VersionId>) -> Value {
    json!({
        "version_id": v.id,
        "restored_from": v.restored_from,
        "previous_version_id": previous,
    })
}

/// The data of `version.labeled`.
pub fn version_labeled_data(v: VersionId, old: Option<&str>, new: Option<&str>) -> Value {
    json!({ "version_id": v, "old_label": old, "new_label": new })
}

/// The data of `node.created` for a file made by an upload.
pub fn node_created_data(
    parent: crate::ids::NodeId,
    name: &str,
    content_type: Option<&str>,
) -> Value {
    json!({
        "parent_id": parent,
        "name": name,
        "kind": "file",
        "type": content_type,
        "link_kind": null,
        "link_target": null,
    })
}

/// The cursor of a version page: the position after the last version (`created_at`, `id`).
pub fn version_cursor(v: &Version) -> crate::paging::Cursor {
    use base64::Engine as _;
    crate::paging::Cursor(
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(
            "v1.{}.{}",
            v.created_at.unix_timestamp_nanos(),
            v.id
        )),
    )
}

/// The position of a version cursor. Fails with `bad_request` for a cursor of another kind.
pub fn decode_version_cursor(
    c: &crate::paging::Cursor,
) -> Result<(time::OffsetDateTime, VersionId)> {
    use base64::Engine as _;
    let bad = || {
        api(
            ErrorCode::BadRequest,
            "the cursor is not valid",
            "pass the next_cursor of the last page, or no cursor to start over",
        )
    };
    let text = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(c.0.as_bytes())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(bad)?;
    let mut parts = text.splitn(3, '.');
    if parts.next() != Some("v1") {
        return Err(bad());
    }
    let nanos: i128 = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let id: uuid::Uuid = parts.next().and_then(|n| n.parse().ok()).ok_or_else(bad)?;
    let at = time::OffsetDateTime::from_unix_timestamp_nanos(nanos).map_err(|_| bad())?;
    Ok((at, VersionId(id)))
}

/// The name with ` (n)` before the extension, for `on_conflict: rename`.
pub fn numbered_name(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

/// The trash rule of the nodes area for reads of content and versions: a trashed file has left
/// the tree, so only those who may restore it (`manage`, as `NodeStore::get`) read it; everyone
/// else gets `404`, also with `view` or `edit` (security review of 2026-11-06).
pub async fn require_trashed_read(
    authz: &dyn crate::authz::Authz,
    nodes: &dyn crate::nodes::NodeStore,
    ctx: &Context,
    node: crate::ids::NodeId,
    trashed: bool,
) -> Result<()> {
    if !trashed {
        return Ok(());
    }
    crate::authz::require(authz, nodes, ctx, node, Role::Manage)
        .await
        .map_err(|e| match e {
            Error::Api(_) => crate::nodes::rules::node_not_found(),
            other => other,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_resolved_against_the_size() {
        let r = |s, e| Some(ByteRange { start: s, end: e });
        assert_eq!(resolve_range(r(0, 9), 10).unwrap(), None);
        assert_eq!(resolve_range(r(2, 100), 10).unwrap(), r(2, 9));
        assert!(resolve_range(r(10, 12), 10).is_err());
        assert!(resolve_range(r(0, 0), 0).is_err());
        assert_eq!(resolve_range(None, 0).unwrap(), None);
    }

    #[test]
    fn names_are_numbered_before_the_extension() {
        assert_eq!(numbered_name("plan.txt", 2), "plan (2).txt");
        assert_eq!(numbered_name("README", 3), "README (3)");
        assert_eq!(numbered_name(".env", 2), ".env (2)");
    }

    #[test]
    fn file_names_are_checked() {
        assert!(check_file_name("a.txt").is_ok());
        for bad in ["", ".", "..", "a/b", " a", "a ", "a\0b"] {
            assert!(check_file_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_file_name(&"x".repeat(256)).is_err());
    }
}
