//! Open files: a reader over [`Transfers::open_content`] with ranges, and a writer that spools
//! the body of a PUT and stores it as one new version through the upload flow.
//!
//! [`Transfers::open_content`]: ruum_core::blobs::Transfers::open_content

use crate::error::fs_error;
use crate::fs::Meta;
use bytes::{Buf, Bytes};
use dav_server::fs::{DavFile, DavMetaData, FsError, FsFuture, FsResult};
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use ruum_core::Services;
use ruum_core::actor::Context;
use ruum_core::blobs::{ByteRange, ChunkRef, ChunkingParams, Hash, NewUpload, UploadTarget};
use ruum_core::nodes::{Node, OnConflict};
use ruum_core::tenants::Plan;
use ruum_core::tenants::limits::{plan_limits, tenant_limits};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use tokio::io::AsyncWriteExt;

// The chunks follow `ruum_core::blobs::chunking_for`, so a file gets the same chunks through
// WebDAV and through REST or the CLI.

// ------------------------------------------------------------------------------------- limits

/// The plan limits that apply to one upload, read before the body is spooled.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UploadLimits {
    /// The largest file of the plan (`max_file_bytes`).
    pub max_file: u64,
    /// The free storage of the tenant, if the caller may read the usage (members). For others
    /// the upload flow still checks the quota when the upload is created.
    pub free: Option<u64>,
}

impl UploadLimits {
    /// Refuses a body of `size` bytes that replaces `old` bytes: `413` over the largest file,
    /// `507` over the free storage.
    pub fn check(&self, size: u64, old: u64) -> FsResult<()> {
        if size > self.max_file {
            return Err(FsError::TooLarge);
        }
        if self
            .free
            .is_some_and(|free| size > free.saturating_add(old))
        {
            return Err(FsError::InsufficientStorage);
        }
        Ok(())
    }
}

/// The upload limits of the tenant of `ctx`. When the plan cannot be read, the largest file of
/// any plan is the limit, so the spool is still bounded.
pub(crate) async fn upload_limits(s: &Services, ctx: &Context) -> UploadLimits {
    let Ok(tenant) = s.tenants.get(ctx).await else {
        let max = [Plan::Free, Plan::Personal, Plan::Team]
            .into_iter()
            .map(|p| plan_limits(p).max_file_bytes)
            .max()
            .unwrap_or(0);
        return UploadLimits {
            max_file: max.max(0) as u64,
            free: None,
        };
    };
    match s.tenants.usage(ctx).await {
        Ok(usage) => {
            let l = tenant_limits(tenant.plan, usage.members);
            UploadLimits {
                max_file: l.max_file_bytes.max(0) as u64,
                free: Some(l.storage_bytes.saturating_sub(usage.storage_bytes).max(0) as u64),
            }
        }
        Err(_) => UploadLimits {
            max_file: plan_limits(tenant.plan).max_file_bytes.max(0) as u64,
            free: None,
        },
    }
}

// ------------------------------------------------------------------------------------- reader

type ByteStream = BoxStream<'static, ruum_core::Result<Bytes>>;

/// A file opened for reading. It opens the content stream at the current position on the first
/// read after a seek, so a ranged GET reads only the bytes of the range.
pub(crate) struct Reader {
    s: Services,
    ctx: Context,
    node: Node,
    pos: u64,
    // In a mutex only to make the reader `Sync`; it is always used through `&mut self`.
    stream: Mutex<Option<ByteStream>>,
    pending: Bytes,
}

impl std::fmt::Debug for Reader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reader")
            .field("node", &self.node.id)
            .field("pos", &self.pos)
            .finish()
    }
}

impl Reader {
    pub fn new(s: Services, ctx: Context, node: Node) -> Self {
        Reader {
            s,
            ctx,
            node,
            pos: 0,
            stream: Mutex::new(None),
            pending: Bytes::new(),
        }
    }

    fn len(&self) -> u64 {
        Meta::of(&self.node).len
    }

    async fn read(&mut self, count: usize) -> FsResult<Bytes> {
        let len = self.len();
        if self.pos >= len || count == 0 || self.node.head_version.is_none() {
            return Ok(Bytes::new());
        }
        if self.pending.is_empty() {
            let stream = self.stream.get_mut().map_err(|_| FsError::GeneralFailure)?;
            if stream.is_none() {
                let content = self
                    .s
                    .transfers
                    .open_content(
                        &self.ctx,
                        self.node.id,
                        self.node.head_version,
                        Some(ByteRange {
                            start: self.pos,
                            end: len - 1,
                        }),
                    )
                    .await
                    .map_err(|e| fs_error(&e))?;
                *stream = Some(content.body);
            }
            let Some(s) = stream.as_mut() else {
                return Err(FsError::GeneralFailure);
            };
            match s.next().await {
                Some(Ok(b)) => self.pending = b,
                Some(Err(e)) => return Err(fs_error(&e)),
                None => return Ok(Bytes::new()),
            }
        }
        let n = count.min(self.pending.len());
        let out = self.pending.split_to(n);
        self.pos += n as u64;
        Ok(out)
    }
}

impl DavFile for Reader {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let m: Box<dyn DavMetaData> = Box::new(Meta::of(&self.node));
        async move { Ok(m) }.boxed()
    }

    fn write_buf(&'_ mut self, _buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        async { Err(FsError::Forbidden) }.boxed()
    }

    fn write_bytes(&'_ mut self, _buf: Bytes) -> FsFuture<'_, ()> {
        async { Err(FsError::Forbidden) }.boxed()
    }

    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, Bytes> {
        self.read(count).boxed()
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        async move {
            let len = self.len() as i128;
            let new = match pos {
                SeekFrom::Start(n) => n as i128,
                SeekFrom::End(d) => len + d as i128,
                SeekFrom::Current(d) => self.pos as i128 + d as i128,
            };
            if new < 0 {
                return Err(FsError::GeneralFailure);
            }
            let new = new as u64;
            if new != self.pos {
                self.pos = new;
                self.pending = Bytes::new();
                *self.stream.get_mut().map_err(|_| FsError::GeneralFailure)? = None;
            }
            Ok(new)
        }
        .boxed()
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        async { Ok(()) }.boxed()
    }
}

// ------------------------------------------------------------------------------------- writer

enum Target {
    Existing(Node),
    New { parent: Node, name: String },
}

/// A file opened for a PUT. The body goes to an anonymous temporary file; `flush` (called once,
/// after the whole body) stores it as one new version: a new file in the parent, or a new head
/// version of the existing file.
pub(crate) struct Writer {
    s: Services,
    ctx: Context,
    target: Target,
    /// The `Content-Length` of the request, if known. A shorter body stores nothing.
    expected: Option<u64>,
    /// The largest body to accept; a longer one fails with `413` while it is written.
    max: u64,
    /// Where the body is spooled (`None`: the system temporary folder).
    spool_dir: Option<PathBuf>,
    spool: Option<tokio::fs::File>,
    written: u64,
    done: Option<Node>,
    started: SystemTime,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer")
            .field("written", &self.written)
            .field("done", &self.done.as_ref().map(|n| n.id))
            .finish()
    }
}

/// The options of one writer.
#[derive(Clone, Debug)]
pub(crate) struct WriteOpts {
    pub expected: Option<u64>,
    pub max: u64,
    pub spool_dir: Option<PathBuf>,
}

impl Writer {
    pub fn existing(s: Services, ctx: Context, node: Node, opts: WriteOpts) -> Self {
        Self::with(s, ctx, Target::Existing(node), opts)
    }

    pub fn new_file(
        s: Services,
        ctx: Context,
        parent: Node,
        name: String,
        opts: WriteOpts,
    ) -> Self {
        Self::with(s, ctx, Target::New { parent, name }, opts)
    }

    /// A writer for a file that is already stored (an empty file made by LOCK): it takes no
    /// bytes, and `flush` stores nothing.
    pub fn stored(s: Services, ctx: Context, node: Node) -> Self {
        let mut w = Self::with(
            s,
            ctx,
            Target::Existing(node.clone()),
            WriteOpts {
                expected: Some(0),
                max: 0,
                spool_dir: None,
            },
        );
        w.done = Some(node);
        w
    }

    fn with(s: Services, ctx: Context, target: Target, opts: WriteOpts) -> Self {
        Writer {
            s,
            ctx,
            target,
            expected: opts.expected,
            max: opts.max,
            spool_dir: opts.spool_dir,
            spool: None,
            written: 0,
            done: None,
            started: SystemTime::now(),
        }
    }

    /// The stored node, after `flush`.
    pub fn into_node(self) -> Option<Node> {
        self.done
    }

    fn name(&self) -> &str {
        match &self.target {
            Target::Existing(n) => &n.name,
            Target::New { name, .. } => name,
        }
    }

    async fn write(&mut self, data: &[u8]) -> FsResult<()> {
        if self.done.is_some() {
            return Err(FsError::GeneralFailure);
        }
        if data.is_empty() {
            return Ok(());
        }
        if self.written.saturating_add(data.len() as u64) > self.max {
            // Stop before the disk fills: the body is over the largest file of the plan.
            self.spool = None;
            return Err(FsError::TooLarge);
        }
        if self.spool.is_none() {
            let dir = self.spool_dir.clone();
            let f = tokio::task::spawn_blocking(move || match dir {
                Some(d) => tempfile::tempfile_in(d),
                None => tempfile::tempfile(),
            })
            .await
            .map_err(|_| FsError::GeneralFailure)?
            .map_err(|e| {
                tracing::error!(error = %e, "webdav: cannot make a spool file");
                FsError::InsufficientStorage
            })?;
            self.spool = Some(tokio::fs::File::from_std(f));
        }
        let Some(spool) = self.spool.as_mut() else {
            return Err(FsError::GeneralFailure);
        };
        spool.write_all(data).await.map_err(|e| {
            tracing::error!(error = %e, "webdav: cannot write the spool file");
            FsError::InsufficientStorage
        })?;
        self.written += data.len() as u64;
        Ok(())
    }

    async fn commit(&mut self) -> FsResult<()> {
        if self.done.is_some() {
            return Ok(());
        }
        if self.expected.is_some_and(|n| n != self.written) {
            // The body ended early: store nothing (dav-server then answers 400).
            return Err(FsError::GeneralFailure);
        }
        let content_type = content_type(self.name());
        let params = ruum_core::blobs::chunking_for(self.written, Some(&content_type));
        let (chunks, file) = match self.spool.take() {
            None => (Vec::new(), None),
            Some(mut f) => {
                f.flush().await.map_err(|_| FsError::GeneralFailure)?;
                let std = f.into_std().await;
                let p = params.clone();
                tokio::task::spawn_blocking(move || cut(std, &p))
                    .await
                    .map_err(|_| FsError::GeneralFailure)??
            }
        };
        let content_hash = match &file {
            None => Hash(*blake3::hash(&[]).as_bytes()),
            Some(f) => f.hash,
        };
        let target = match &self.target {
            Target::Existing(n) => UploadTarget::NewVersion {
                node_id: n.id,
                base_version: None,
            },
            Target::New { parent, name } => UploadTarget::NewFile {
                parent_id: parent.id,
                name: name.clone(),
                on_conflict: OnConflict::Fail,
            },
        };
        let t = &self.s.transfers;
        let upload = t
            .create_upload(
                &self.ctx,
                NewUpload {
                    target,
                    content_type: Some(content_type),
                    chunking: params.id.to_string(),
                    size: self.written,
                    content_hash,
                    chunks: chunks.iter().map(|c| c.chunk).collect(),
                },
            )
            .await
            .map_err(|e| fs_error(&e))?;
        let result = async {
            if let Some(f) = file {
                let at: HashMap<Hash, &Cut> = chunks.iter().map(|c| (c.chunk.hash, c)).collect();
                for hash in &upload.missing {
                    let Some(c) = at.get(hash) else {
                        return Err(FsError::GeneralFailure);
                    };
                    let data = f.read(c.offset, c.chunk.size as usize).await?;
                    t.put_chunk(&self.ctx, upload.id, *hash, data)
                        .await
                        .map_err(|e| fs_error(&e))?;
                }
            }
            t.complete_upload(&self.ctx, upload.id)
                .await
                .map_err(|e| fs_error(&e))
        }
        .await;
        match result {
            Ok(w) => {
                self.done = Some(w.value.0);
                Ok(())
            }
            Err(e) => {
                if let Err(abort) = t.abort_upload(&self.ctx, upload.id).await {
                    tracing::warn!(error = %abort, "webdav: cannot abort an upload");
                }
                Err(e)
            }
        }
    }
}

impl DavFile for Writer {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let m: Box<dyn DavMetaData> = match &self.done {
            Some(n) => Box::new(Meta::of(n)),
            None => Box::new(Meta {
                dir: false,
                len: self.written,
                modified: self.started,
                created: self.started,
                etag: String::new(),
            }),
        };
        async move { Ok(m) }.boxed()
    }

    fn write_buf(&'_ mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        async move {
            while buf.has_remaining() {
                let n = {
                    let chunk = buf.chunk();
                    let n = chunk.len();
                    let data = chunk.to_vec();
                    self.write(&data).await?;
                    n
                };
                buf.advance(n);
            }
            Ok(())
        }
        .boxed()
    }

    fn write_bytes(&'_ mut self, buf: Bytes) -> FsFuture<'_, ()> {
        async move { self.write(&buf).await }.boxed()
    }

    fn read_bytes(&'_ mut self, _count: usize) -> FsFuture<'_, Bytes> {
        async { Err(FsError::Forbidden) }.boxed()
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        // Only "where am I" seeks; partial writes are refused when the file is opened.
        let here = self.written;
        async move {
            match pos {
                SeekFrom::Start(n) if n == here => Ok(here),
                SeekFrom::Current(0) => Ok(here),
                _ => Err(FsError::NotImplemented),
            }
        }
        .boxed()
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        self.commit().boxed()
    }
}

/// One chunk of the spooled body and where it starts.
struct Cut {
    chunk: ChunkRef,
    offset: u64,
}

/// The spooled body after chunking.
struct Spooled {
    file: Mutex<std::fs::File>,
    hash: Hash,
}

impl Spooled {
    async fn read(&self, offset: u64, len: usize) -> FsResult<Bytes> {
        let f = {
            let f = self.file.lock().map_err(|_| FsError::GeneralFailure)?;
            f.try_clone().map_err(|_| FsError::GeneralFailure)?
        };
        tokio::task::spawn_blocking(move || {
            let mut f = f;
            f.seek(SeekFrom::Start(offset))?;
            let mut buf = vec![0u8; len];
            f.read_exact(&mut buf)?;
            Ok::<_, std::io::Error>(Bytes::from(buf))
        })
        .await
        .map_err(|_| FsError::GeneralFailure)?
        .map_err(|_| FsError::GeneralFailure)
    }
}

/// Cuts the spooled body into chunks with FastCDC 2020 and the given parameters, and hashes the
/// chunks and the whole content with BLAKE3.
fn cut(mut file: std::fs::File, p: &ChunkingParams) -> FsResult<(Vec<Cut>, Option<Spooled>)> {
    file.seek(SeekFrom::Start(0))
        .map_err(|_| FsError::GeneralFailure)?;
    let reader = file.try_clone().map_err(|_| FsError::GeneralFailure)?;
    let mut whole = blake3::Hasher::new();
    let mut out = Vec::new();
    let cdc = fastcdc::v2020::StreamCDC::new(
        reader,
        p.min_size as usize,
        p.avg_size as usize,
        p.max_size as usize,
    );
    for c in cdc {
        let c = c.map_err(|_| FsError::GeneralFailure)?;
        whole.update(&c.data);
        out.push(Cut {
            chunk: ChunkRef {
                hash: Hash(*blake3::hash(&c.data).as_bytes()),
                size: c.length as u32,
            },
            offset: c.offset,
        });
    }
    Ok((
        out,
        Some(Spooled {
            file: Mutex::new(file),
            hash: Hash(*whole.finalize().as_bytes()),
        }),
    ))
}

/// Writes the head content of the file `from` as a new head version of the file `to`, through
/// the upload flow. Chunks the tenant already has are not stored again, so for the usual case
/// (the same chunking) no chunk is written. Returns `to` after the write.
///
/// The core traits have no call that reads the chunk list of a version, so the content is read
/// and cut again here; with one, this could send the chunk list without reading the bytes.
pub(crate) async fn copy_content(
    s: &Services,
    ctx: &Context,
    from: &Node,
    to: Node,
    opts: WriteOpts,
) -> FsResult<Node> {
    let mut w = Writer::existing(s.clone(), ctx.clone(), to, opts);
    if from.head_version.is_some() {
        let mut content = s
            .transfers
            .open_content(ctx, from.id, from.head_version, None)
            .await
            .map_err(|e| fs_error(&e))?;
        w.expected = Some(content.total_size);
        while let Some(b) = content.body.next().await {
            w.write(&b.map_err(|e| fs_error(&e))?).await?;
        }
    } else {
        w.expected = Some(0);
    }
    w.commit().await?;
    w.into_node().ok_or(FsError::GeneralFailure)
}

/// The MIME type of a file, from the extension of its name.
pub(crate) fn content_type(name: &str) -> String {
    mime_guess::from_path(name)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruum_core::blobs::{DEFAULT_CHUNKING, SMALL_CHUNKING};
    use std::io::Write;

    #[test]
    fn cuts_and_hashes_like_one_pass() {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&data).unwrap();
        let (cuts, spooled) = cut(f, &SMALL_CHUNKING).unwrap();
        let spooled = spooled.unwrap();
        assert_eq!(spooled.hash.0, *blake3::hash(&data).as_bytes());
        assert!(cuts.len() > 1);
        let mut at = 0u64;
        for c in &cuts {
            assert_eq!(c.offset, at);
            let part = &data[at as usize..at as usize + c.chunk.size as usize];
            assert_eq!(c.chunk.hash.0, *blake3::hash(part).as_bytes());
            assert!(c.chunk.size <= SMALL_CHUNKING.max_size);
            at += c.chunk.size as u64;
        }
        assert_eq!(at, data.len() as u64);
    }

    #[test]
    fn picks_chunks_like_the_blobs_area() {
        assert_eq!(
            ruum_core::blobs::chunking_for(10, Some("image/png")).id,
            SMALL_CHUNKING.id
        );
        assert_eq!(
            ruum_core::blobs::chunking_for(4 << 20, None).id,
            SMALL_CHUNKING.id
        );
        assert_eq!(
            ruum_core::blobs::chunking_for(10 << 20, Some("text/plain")).id,
            SMALL_CHUNKING.id
        );
        assert_eq!(
            ruum_core::blobs::chunking_for(10 << 20, Some("application/json; charset=utf-8")).id,
            SMALL_CHUNKING.id
        );
        assert_eq!(
            ruum_core::blobs::chunking_for(100 << 20, Some("text/plain")).id,
            DEFAULT_CHUNKING.id
        );
        assert_eq!(
            ruum_core::blobs::chunking_for(100 << 20, Some("video/mp4")).id,
            DEFAULT_CHUNKING.id
        );
        assert_eq!(
            ruum_core::blobs::chunking_for(10 << 20, Some("application/zip")).id,
            DEFAULT_CHUNKING.id
        );
        assert_eq!(content_type("a.txt"), "text/plain");
        assert_eq!(content_type("noext"), "application/octet-stream");
    }

    #[test]
    fn limits_refuse_large_bodies_and_full_quotas() {
        let l = UploadLimits {
            max_file: 100,
            free: Some(50),
        };
        assert!(l.check(50, 0).is_ok());
        assert!(matches!(l.check(101, 0), Err(FsError::TooLarge)));
        assert!(matches!(l.check(60, 0), Err(FsError::InsufficientStorage)));
        // A new version replaces the old content.
        assert!(l.check(60, 20).is_ok());
        let guest = UploadLimits {
            max_file: 100,
            free: None,
        };
        assert!(guest.check(100, 0).is_ok());
    }
}
