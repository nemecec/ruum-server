//! Blobs (`docs/DESIGN.md`, section 5.1): content-defined chunks, manifests, the blob store and
//! resumable uploads and ranged downloads.
//!
//! - Files are cut into chunks with FastCDC (about 1-4 MiB). Each chunk is addressed by its
//!   BLAKE3 hash and stored once per tenant at `/<tenant>/<hash-prefix>/<hash>`. No dedup across
//!   tenants.
//! - A version is a manifest: the ordered list of chunk hashes.
//! - Uploads are resumable: the client sends the hashes first and uploads only missing chunks.

pub mod memory;

use crate::actor::{Actor, Context};
use crate::error::Result;
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
    pub fn of(_data: &[u8]) -> Self {
        todo!("blobs: Hash::of")
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(_s: &str) -> Result<Self> {
        todo!("blobs: Hash::from_hex")
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
pub fn chunking_for(_size: u64, _content_type: Option<&str>) -> ChunkingParams {
    todo!("blobs: chunking_for")
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
    pub fn encode(&self) -> Vec<u8> {
        todo!("blobs: Manifest::encode")
    }

    /// Decodes `versions.manifest` with the format of `versions.encoding`.
    pub fn decode(_format: ManifestFormat, _bytes: &[u8]) -> Result<Self> {
        todo!("blobs: Manifest::decode")
    }
}

/// Cuts data into chunks with the given parameters.
pub fn chunk(_data: &[u8], _params: ChunkingParams) -> Vec<ChunkRef> {
    todo!("blobs: chunk")
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
    pub version_id: VersionId,
    pub total_size: u64,
    pub range: Option<ByteRange>,
    pub content_type: String,
    pub content_hash: Hash,
    pub body: BoxStream<'static, Result<Bytes>>,
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
    /// Opens a version (the head version when `version` is `None`) for reading.
    async fn open_content(
        &self,
        ctx: &Context,
        node: NodeId,
        version: Option<VersionId>,
        range: Option<ByteRange>,
    ) -> Result<Content>;
}
