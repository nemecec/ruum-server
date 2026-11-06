//! The chunked upload (`POST /v1/uploads`, design section 5.1): the CLI cuts the file with
//! FastCDC (the parameter set from `GET /v1/uploads/params`), hashes each chunk and the whole
//! content with BLAKE3, creates the upload, sends only the chunks the server does not have
//! (`missing`), and completes it. Files of any size work; equal chunks are sent once.

use crate::client::{Body, Client};
use anyhow::{Context as _, Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// One chunk of the local file.
struct Chunk {
    offset: u64,
    size: usize,
    hash: String,
}

/// The result of an upload.
pub struct Uploaded {
    /// The `ContentWrite` answer of `completeUpload` (node, version, change).
    pub write: Value,
    pub size: u64,
    pub chunks: usize,
    /// The chunks that were sent (the server had the others).
    pub sent: usize,
}

/// Uploads `local` to `target` (an `UploadTarget` of the contract).
pub async fn upload_file(c: &mut Client, local: &Path, target: Value) -> Result<Uploaded> {
    let meta = std::fs::metadata(local).with_context(|| format!("read {}", local.display()))?;
    if !meta.is_file() {
        bail!("{}: not a file", local.display());
    }
    let size = meta.len();
    let params = c
        .get("/v1/uploads/params", &[("size", size.to_string())])
        .await?;
    let id = params["id"]
        .as_str()
        .ok_or_else(|| anyhow!("the chunking parameters have no id"))?
        .to_string();
    let algorithm = params["algorithm"].as_str().unwrap_or_default();
    if algorithm != "fastcdc-2020" {
        bail!(
            "the server asks for chunking {algorithm}, which this CLI does not know\nhint: update the ruum CLI"
        );
    }
    let num = |k: &str| -> Result<usize> {
        params[k]
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| anyhow!("the chunking parameters have no {k}"))
    };
    let (min, avg, max) = (num("min_size")?, num("avg_size")?, num("max_size")?);

    let (chunks, content_hash) = cut(local, min, avg, max)?;
    let mut body = target;
    body["chunking"] = Value::String(id);
    body["size"] = json!(size);
    body["content_hash"] = Value::String(content_hash);
    body["chunks"] = Value::Array(
        chunks
            .iter()
            .map(|c| json!({"hash": c.hash, "size": c.size}))
            .collect(),
    );
    let upload = c.post("/v1/uploads", body).await?;
    let upload_id = upload["id"]
        .as_str()
        .ok_or_else(|| anyhow!("the upload has no id"))?
        .to_string();

    let missing: HashSet<String> = upload["missing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|h| h.as_str().map(str::to_string))
        .collect();
    let mut sent = HashSet::new();
    let mut file = std::fs::File::open(local)?;
    for chunk in &chunks {
        if !missing.contains(&chunk.hash) || sent.contains(&chunk.hash) {
            continue;
        }
        let mut buf = vec![0u8; chunk.size];
        file.seek(SeekFrom::Start(chunk.offset))?;
        file.read_exact(&mut buf)?;
        let r = c
            .send(
                Method::PUT,
                &format!("/v1/uploads/{upload_id}/chunks/{}", chunk.hash),
                &[],
                Body::Bytes(buf),
                &[],
            )
            .await;
        if let Err(e) = r {
            // Leave no open upload behind.
            let _ = c
                .send(
                    Method::DELETE,
                    &format!("/v1/uploads/{upload_id}"),
                    &[],
                    Body::None,
                    &[],
                )
                .await;
            return Err(e);
        }
        sent.insert(chunk.hash.clone());
    }
    let write = c
        .json(
            Method::POST,
            &format!("/v1/uploads/{upload_id}/complete"),
            &[],
            Body::None,
        )
        .await?;
    Ok(Uploaded {
        write,
        size,
        chunks: chunks.len(),
        sent: sent.len(),
    })
}

/// Cuts the file into chunks and hashes them and the whole content.
fn cut(local: &Path, min: usize, avg: usize, max: usize) -> Result<(Vec<Chunk>, String)> {
    let file = std::fs::File::open(local).with_context(|| format!("read {}", local.display()))?;
    let mut whole = blake3::Hasher::new();
    let mut chunks = Vec::new();
    for c in fastcdc::v2020::StreamCDC::new(file, min, avg, max) {
        let c = c.map_err(|e| anyhow!("read {}: {e}", local.display()))?;
        whole.update(&c.data);
        chunks.push(Chunk {
            offset: c.offset,
            size: c.length,
            hash: blake3::hash(&c.data).to_hex().to_string(),
        });
    }
    Ok((chunks, whole.finalize().to_hex().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_cover_the_file_and_hash_it() {
        let dir = std::env::temp_dir().join(format!("ruum-cli-cut-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("data.bin");
        let data: Vec<u8> = (0..300_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        std::fs::write(&p, &data).unwrap();
        let (chunks, hash) = cut(&p, 16 * 1024, 32 * 1024, 64 * 1024).unwrap();
        assert!(chunks.len() > 2);
        assert_eq!(chunks.iter().map(|c| c.size).sum::<usize>(), data.len());
        assert_eq!(hash, blake3::hash(&data).to_hex().to_string());
        let first = &chunks[0];
        assert_eq!(
            first.hash,
            blake3::hash(&data[..first.size]).to_hex().to_string()
        );
        std::fs::remove_dir_all(&dir).unwrap();
        // An empty file has no chunks.
        let e = dir.with_extension("empty");
        std::fs::write(&e, b"").unwrap();
        let (chunks, hash) = cut(&e, 16 * 1024, 32 * 1024, 64 * 1024).unwrap();
        assert!(chunks.is_empty());
        assert_eq!(hash, blake3::hash(b"").to_hex().to_string());
        std::fs::remove_file(&e).unwrap();
    }
}
