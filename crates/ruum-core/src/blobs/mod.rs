//! Blobs (`docs/DESIGN.md`, section 5.1): content-defined chunks, manifests, the blob store and
//! resumable uploads and ranged downloads.
//!
//! - Files are cut into chunks with FastCDC (about 1-4 MiB). Each chunk is addressed by its
//!   BLAKE3 hash and stored once per tenant at `/<tenant>/<hash-prefix>/<hash>`. No dedup across
//!   tenants.
//! - A version is a manifest: the ordered list of chunk hashes.
//! - Uploads are resumable: the client sends the hashes first and uploads only missing chunks.

pub mod gc;
pub mod logic;
pub mod memory;
pub mod object;
pub mod postgres;
pub mod setup;
pub mod signed;

#[cfg(test)]
mod conformance;
/// Test helpers (a small node store and a test world). Not for production use.
#[doc(hidden)]
pub mod testing;
#[cfg(test)]
mod tests;

pub use signed::{CONTENT_URL_SECONDS, ContentClaims, ContentSigner, Disposition};

use crate::actor::{Actor, Context};
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TenantId, UploadId, VersionId};
use crate::nodes::{Node, OnConflict};
use crate::versions::Version;
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use time::OffsetDateTime;

/// The BLAKE3 hash of a chunk or of a whole content. Shown as 64 lowercase hex characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Hashes bytes with BLAKE3.
    pub fn of(data: &[u8]) -> Self {
        Hash(*blake3::hash(data).as_bytes())
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parses 64 lowercase hex characters. Fails with `validation_failed` otherwise.
    pub fn from_hex(s: &str) -> Result<Self> {
        let bad = || {
            Error::Api(
                ApiError::new(
                    ErrorCode::ValidationFailed,
                    format!(
                        "`{}` is not a BLAKE3 hash",
                        s.chars().take(80).collect::<String>()
                    ),
                )
                .with_hint("a hash is 64 lowercase hex characters"),
            )
        };
        if s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|_| bad())?;
        Ok(Hash(out))
    }
}

impl Serialize for Hash {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let mut out = [0u8; 32];
        hex::decode_to_slice(&s, &mut out).map_err(serde::de::Error::custom)?;
        Ok(Hash(out))
    }
}

/// The chunking algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkAlgorithm {
    /// FastCDC as in the 2020 paper (the `fastcdc::v2020` module).
    #[serde(rename = "fastcdc-2020")]
    Fastcdc2020,
}

/// The chunking parameters. Clients must cut with the same parameters as the server expects, so
/// the server publishes them before the upload (`GET /v1/uploads/params`, [`chunking_for`]) and
/// the client names the set it used (`id`) in `POST /v1/uploads`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkingParams {
    /// A stable name of this set, for example `fastcdc-2020-1m-2m-4m`.
    pub id: Cow<'static, str>,
    pub algorithm: ChunkAlgorithm,
    pub min_size: u32,
    pub avg_size: u32,
    pub max_size: u32,
}

/// The default chunking: FastCDC 2020, 1 MiB minimum, 2 MiB average, 4 MiB maximum.
pub const DEFAULT_CHUNKING: ChunkingParams = ChunkingParams {
    id: Cow::Borrowed("fastcdc-2020-1m-2m-4m"),
    algorithm: ChunkAlgorithm::Fastcdc2020,
    min_size: 1024 * 1024,
    avg_size: 2 * 1024 * 1024,
    max_size: 4 * 1024 * 1024,
};

/// Small chunks for text and small files (section 5.3, part 1): FastCDC 2020, 16 KiB minimum,
/// 32 KiB average, 64 KiB maximum.
pub const SMALL_CHUNKING: ChunkingParams = ChunkingParams {
    id: Cow::Borrowed("fastcdc-2020-16k-32k-64k"),
    algorithm: ChunkAlgorithm::Fastcdc2020,
    min_size: 16 * 1024,
    avg_size: 32 * 1024,
    max_size: 64 * 1024,
};

/// The chunking parameters for a file of this size and type (`GET /v1/uploads/params`). They
/// depend only on these inputs and the server version.
///
/// - Small files (at most [`SMALL_FILE_BYTES`]) and text files up to [`SMALL_TEXT_BYTES`] use
///   [`SMALL_CHUNKING`], so a small edit adds little.
/// - Everything else uses [`DEFAULT_CHUNKING`] (1-4 MiB chunks), also already compressed
///   content (JPEG, video, ZIP).
pub fn chunking_for(size: u64, content_type: Option<&str>) -> ChunkingParams {
    if size <= SMALL_FILE_BYTES || (size <= SMALL_TEXT_BYTES && is_text(content_type)) {
        SMALL_CHUNKING
    } else {
        DEFAULT_CHUNKING
    }
}

/// Files up to this size always use [`SMALL_CHUNKING`].
pub const SMALL_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Text files up to this size use [`SMALL_CHUNKING`].
pub const SMALL_TEXT_BYTES: u64 = 64 * 1024 * 1024;

/// Whether the MIME type is text-like (text, source, JSON, XML, CSV, YAML, SVG).
pub fn is_text(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else {
        return false;
    };
    let ct = ct
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.ends_with("+json")
        || ct.ends_with("+xml")
        || matches!(
            ct.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-yaml"
                | "application/yaml"
                | "application/toml"
                | "application/x-sh"
                | "application/sql"
                | "application/csv"
        )
}

/// The chunking parameters with this `id`, if the server publishes them.
pub fn chunking_by_id(id: &str) -> Option<ChunkingParams> {
    [DEFAULT_CHUNKING, SMALL_CHUNKING]
        .into_iter()
        .find(|p| p.id == id)
}

/// The format of an encoded manifest (`versions.encoding`). Step 2 adds typed-node manifests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestFormat {
    /// An ordered list of chunk references.
    #[default]
    #[serde(rename = "chunks_v1")]
    ChunksV1,
}

impl ManifestFormat {
    /// The value of `versions.encoding`.
    pub fn as_str(self) -> &'static str {
        match self {
            ManifestFormat::ChunksV1 => "chunks_v1",
        }
    }
}

/// One chunk in a manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRef {
    pub hash: Hash,
    pub size: u32,
}

/// The content of one file version: the ordered chunks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format; stored in `versions.encoding` next to the encoded bytes.
    #[serde(default)]
    pub format: ManifestFormat,
    /// The `id` of the chunking parameters that cut the content.
    pub chunking: String,
    pub chunks: Vec<ChunkRef>,
    pub size: u64,
    /// BLAKE3 of the whole content.
    pub content_hash: Hash,
}

impl Manifest {
    /// Encodes the manifest for `versions.manifest` (in the format `self.format`).
    ///
    /// `chunks_v1` is binary, little endian: the version byte `1`, the length of `chunking`
    /// (one byte) and its bytes, `size` (u64), `content_hash` (32 bytes), the chunk count (u32),
    /// then per chunk its hash (32 bytes) and size (u32).
    pub fn encode(&self) -> Vec<u8> {
        match self.format {
            ManifestFormat::ChunksV1 => {
                let id = self.chunking.as_bytes();
                let id = &id[..id.len().min(255)];
                let mut out = Vec::with_capacity(48 + id.len() + self.chunks.len() * 36);
                out.push(1u8);
                out.push(id.len() as u8);
                out.extend_from_slice(id);
                out.extend_from_slice(&self.size.to_le_bytes());
                out.extend_from_slice(&self.content_hash.0);
                out.extend_from_slice(&(self.chunks.len() as u32).to_le_bytes());
                for c in &self.chunks {
                    out.extend_from_slice(&c.hash.0);
                    out.extend_from_slice(&c.size.to_le_bytes());
                }
                out
            }
        }
    }

    /// Decodes `versions.manifest` with the format of `versions.encoding`. Checks that the
    /// chunk sizes add up to `size`.
    pub fn decode(format: ManifestFormat, bytes: &[u8]) -> Result<Self> {
        let bad = |what: &str| Error::Internal(format!("bad manifest: {what}"));
        match format {
            ManifestFormat::ChunksV1 => {
                let mut r = Reader(bytes);
                if r.take(1).ok_or_else(|| bad("empty"))? != [1u8] {
                    return Err(bad("unknown version"));
                }
                let n = r.take(1).ok_or_else(|| bad("short"))?[0] as usize;
                let chunking = std::str::from_utf8(r.take(n).ok_or_else(|| bad("short"))?)
                    .map_err(|_| bad("chunking id"))?
                    .to_string();
                let size = u64::from_le_bytes(r.array().ok_or_else(|| bad("short"))?);
                let content_hash = Hash(r.array().ok_or_else(|| bad("short"))?);
                let count = u32::from_le_bytes(r.array().ok_or_else(|| bad("short"))?) as usize;
                if r.0.len() != count.saturating_mul(36) {
                    return Err(bad("chunk list length"));
                }
                let mut chunks = Vec::with_capacity(count);
                let mut total: u64 = 0;
                for _ in 0..count {
                    let hash = Hash(r.array().ok_or_else(|| bad("short"))?);
                    let size = u32::from_le_bytes(r.array().ok_or_else(|| bad("short"))?);
                    total += u64::from(size);
                    chunks.push(ChunkRef { hash, size });
                }
                if total != size {
                    return Err(bad("chunk sizes do not add up"));
                }
                Ok(Manifest {
                    format,
                    chunking,
                    chunks,
                    size,
                    content_hash,
                })
            }
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N).and_then(|s| s.try_into().ok())
    }
}

/// Cuts data into chunks with the given parameters (FastCDC 2020, normalization level 1, the
/// defaults of the `fastcdc` crate). Empty data has no chunks.
pub fn chunk(data: &[u8], params: ChunkingParams) -> Vec<ChunkRef> {
    match params.algorithm {
        ChunkAlgorithm::Fastcdc2020 => fastcdc::v2020::FastCDC::new(
            data,
            params.min_size as usize,
            params.avg_size as usize,
            params.max_size as usize,
        )
        .map(|c| ChunkRef {
            hash: Hash::of(&data[c.offset..c.offset + c.length]),
            size: c.length as u32,
        })
        .collect(),
    }
}

/// The manifest of `data`, cut with `params`.
pub fn manifest_of(data: &[u8], params: ChunkingParams) -> Manifest {
    Manifest {
        format: ManifestFormat::ChunksV1,
        chunking: params.id.to_string(),
        chunks: chunk(data, params),
        size: data.len() as u64,
        content_hash: Hash::of(data),
    }
}

/// The object storage of chunks. One implementation is in memory, one uses `object_store`
/// (Garage on the laptop, Hetzner Object Storage in production).
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Stores a chunk. Storing a chunk that exists is a no-op. The caller has checked the hash.
    async fn put_chunk(&self, tenant: TenantId, hash: Hash, data: Bytes) -> Result<()>;
    async fn get_chunk(&self, tenant: TenantId, hash: Hash) -> Result<Bytes>;
    /// For each hash, whether the tenant has the chunk.
    async fn has_chunks(&self, tenant: TenantId, hashes: &[Hash]) -> Result<Vec<bool>>;
    /// Deletes a chunk. Only garbage collection calls this.
    async fn delete_chunk(&self, tenant: TenantId, hash: Hash) -> Result<()>;
}

/// The state of an upload session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Open,
    Completed,
    Aborted,
    Expired,
}

/// Where an upload goes: a new version of an existing file, or a new file in a folder.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum UploadTarget {
    NewVersion {
        node_id: NodeId,
        /// Fail with `version_mismatch` if the head version is not this one.
        base_version: Option<VersionId>,
    },
    NewFile {
        parent_id: NodeId,
        name: String,
        #[serde(default)]
        on_conflict: OnConflict,
    },
}

/// A request to start an upload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewUpload {
    #[serde(flatten)]
    pub target: UploadTarget,
    pub content_type: Option<String>,
    /// The `id` of the chunking parameters the client used. Unknown IDs fail with
    /// `validation_failed`.
    pub chunking: String,
    pub size: u64,
    pub content_hash: Hash,
    pub chunks: Vec<ChunkRef>,
}

/// An upload session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Upload {
    pub id: UploadId,
    pub tenant_id: TenantId,
    #[serde(flatten)]
    pub target: UploadTarget,
    pub content_type: Option<String>,
    pub size: u64,
    pub content_hash: Hash,
    pub chunks: Vec<ChunkRef>,
    /// The chunks that the server does not have yet. Known, accepted risk: this tells the caller
    /// whether the tenant stores a chunk, also from a file the caller cannot read (per tenant
    /// only; see `createUpload` in `openapi.yaml`).
    pub missing: Vec<Hash>,
    pub status: UploadStatus,
    pub chunking: ChunkingParams,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub created_by: Actor,
}

/// A byte range of a download, inclusive at both ends, as in HTTP `Range`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// A download: the metadata and a stream of the bytes in the range.
pub struct Content {
    pub node_id: NodeId,
    /// The node name, for `Content-Disposition`.
    pub name: String,
    pub version_id: VersionId,
    pub total_size: u64,
    pub range: Option<ByteRange>,
    pub content_type: String,
    pub content_hash: Hash,
    pub body: BoxStream<'static, Result<Bytes>>,
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Content")
            .field("node_id", &self.node_id)
            .field("version_id", &self.version_id)
            .field("total_size", &self.total_size)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

/// Resumable uploads and ranged downloads.
#[async_trait]
pub trait Transfers: Send + Sync {
    async fn create_upload(&self, ctx: &Context, req: NewUpload) -> Result<Upload>;
    async fn get_upload(&self, ctx: &Context, id: UploadId) -> Result<Upload>;
    /// Stores one chunk of the upload. Fails with `hash_mismatch` if the data has another hash.
    async fn put_chunk(&self, ctx: &Context, id: UploadId, hash: Hash, data: Bytes) -> Result<()>;
    /// Checks that all chunks are there and that the content hash matches, then makes the new
    /// version (and the new file node, for `NewFile`).
    async fn complete_upload(
        &self,
        ctx: &Context,
        id: UploadId,
    ) -> Result<crate::journal::Written<(Node, Version)>>;
    async fn abort_upload(&self, ctx: &Context, id: UploadId) -> Result<()>;
    /// Opens a version (the head version when `version` is `None`) for reading. Needs `view`.
    /// A range past the end fails with `range_not_satisfiable`; an end past the end is cut.
    async fn open_content(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        range: Option<ByteRange>,
    ) -> Result<Content>;
    /// Replaces the content of a file in one request (`putContent`): chunks `data` with
    /// [`chunking_for`], stores the chunks and makes a version (the version policy may merge
    /// it into the head). Needs `edit`. `if_version` is the expected head (`If-Match`); another
    /// head fails with `precondition_failed`.
    async fn put_content(
        &self,
        ctx: &Context,
        node: NodeId,
        data: Bytes,
        content_type: Option<String>,
        if_version: Option<VersionId>,
    ) -> Result<crate::journal::Written<(Node, Version)>>;
    /// A signed grant to read one version on the content domain for
    /// [`CONTENT_URL_SECONDS`] (`getContentUrl`). Needs `view`.
    async fn content_grant(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        disposition: Disposition,
    ) -> Result<(String, ContentClaims)>;
    /// Opens the version of a signed grant (the content domain). The grant is the credential:
    /// no other permission check.
    async fn open_granted(
        &self,
        token: &str,
        range: Option<ByteRange>,
    ) -> Result<(Content, ContentClaims)>;
}

/// The largest body of `putContent` (32 MiB).
pub const MAX_PUT_CONTENT_BYTES: usize = 32 * 1024 * 1024;
/// The largest chunk (`ChunkRef.size.maximum` in `openapi.yaml`).
pub const MAX_CHUNK_BYTES: u32 = 4 * 1024 * 1024;
/// How long an upload session stays open.
pub const UPLOAD_HOURS: i64 = 24;
