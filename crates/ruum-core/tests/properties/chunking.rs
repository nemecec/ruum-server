//! Chunking (design section 5.3; `blobs::chunk`).
//!
//! The model:
//!
//! - The chunks of data cover it exactly: in order, their sizes add up to the length, each
//!   chunk's hash is BLAKE3 of its bytes, every chunk but the last is between the minimum and
//!   the maximum size, and the last is at most the maximum. Joined again they give the same
//!   bytes and the same BLAKE3 as the whole.
//! - Boundaries are content-defined: after an insert at position p, every chunk that ends at or
//!   before p (and starts at least one maximum chunk before the end of the data) keeps its
//!   offset and hash, and every chunk that starts at least [`RESYNC`] after p keeps its hash
//!   and moves by the length of the insert.
//! - A manifest encodes and decodes to itself.
//! - An upload through the content store, chunk by chunk, downloads as the same bytes with the
//!   same BLAKE3.
//!
//! Data is pseudo-random bytes from a seed (xorshift), so a case is cheap to make and to shrink.
//! The set under test is `SMALL_CHUNKING` (16, 32 and 64 KiB); `DEFAULT_CHUNKING` is the same
//! algorithm with sizes too large for a quick test.

use crate::common::{Team, block_on};
use futures::StreamExt;
use proptest::prelude::*;
use proptest::sample::Index;
use ruum_core::blobs::{
    ChunkRef, Hash, Manifest, ManifestFormat, NewUpload, SMALL_CHUNKING, UploadTarget, chunk,
    manifest_of,
};
use ruum_core::nodes::OnConflict;

/// How far after an insert the boundaries must be the old ones again: four maximum chunks.
const RESYNC: usize = 4 * 64 * 1024;

fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// `(offset, chunk)` of each chunk.
fn with_offsets(chunks: &[ChunkRef]) -> Vec<(usize, ChunkRef)> {
    let mut at = 0;
    chunks
        .iter()
        .map(|c| {
            let o = at;
            at += c.size as usize;
            (o, *c)
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn chunks_cover_the_data_and_reassemble(seed in any::<u64>(), len in 0usize..400_000) {
        let p = SMALL_CHUNKING;
        let data = bytes(seed, len);
        let chunks = chunk(&data, p.clone());
        prop_assert_eq!(chunks.iter().map(|c| c.size as usize).sum::<usize>(), len);
        prop_assert_eq!(chunks.is_empty(), len == 0);
        let mut joined = Vec::with_capacity(len);
        for (i, (o, c)) in with_offsets(&chunks).into_iter().enumerate() {
            let piece = &data[o..o + c.size as usize];
            prop_assert_eq!(c.hash, Hash::of(piece), "chunk {} hash", i);
            prop_assert!(c.size <= p.max_size, "chunk {} is {} bytes", i, c.size);
            if i + 1 < chunks.len() {
                prop_assert!(c.size >= p.min_size, "chunk {} is {} bytes", i, c.size);
            }
            joined.extend_from_slice(piece);
        }
        prop_assert!(joined == data, "the joined chunks differ from the data");
        prop_assert_eq!(Hash::of(&joined), Hash::of(&data));
        let m = manifest_of(&data, p);
        prop_assert_eq!(&m.chunks, &chunks);
        prop_assert_eq!(m.content_hash, Hash::of(&data));
        prop_assert_eq!(Manifest::decode(ManifestFormat::ChunksV1, &m.encode()).unwrap(), m);
    }

    #[test]
    fn boundaries_are_stable_under_far_inserts(
        seed in any::<u64>(),
        len in 200_000usize..900_000,
        at in any::<Index>(),
        insert_seed in any::<u64>(),
        insert_len in 1usize..5_000,
    ) {
        let p = SMALL_CHUNKING;
        let max = p.max_size as usize;
        let old = bytes(seed, len);
        let at = at.index(len + 1);
        let insert = bytes(insert_seed ^ 0x9e37_79b9_7f4a_7c15, insert_len);
        let mut new = old[..at].to_vec();
        new.extend_from_slice(&insert);
        new.extend_from_slice(&old[at..]);

        let old_chunks = with_offsets(&chunk(&old, p.clone()));
        let new_chunks = with_offsets(&chunk(&new, p));
        let in_new = |o: usize, c: &ChunkRef| new_chunks.iter().any(|(no, nc)| *no == o && nc == c);
        for (o, c) in &old_chunks {
            let end = o + c.size as usize;
            if end <= at && o + max <= len {
                prop_assert!(in_new(*o, c), "the chunk at {} (before the insert at {}) moved", o, at);
            }
            if *o >= at + RESYNC {
                prop_assert!(
                    in_new(o + insert_len, c),
                    "the chunk at {} (far after the insert at {}) is not at {}", o, at, o + insert_len
                );
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    /// Upload in chunks through the in-memory content store, then download.
    #[test]
    fn uploads_download_as_the_same_bytes(seed in any::<u64>(), len in 1usize..300_000) {
        block_on(async {
            let team = Team::new(&[]).await;
            let ctx = team.owner();
            let s = &team.s;
            let data = bytes(seed, len);
            let m = manifest_of(&data, SMALL_CHUNKING);
            let up = s
                .transfers
                .create_upload(
                    &ctx,
                    NewUpload {
                        target: UploadTarget::NewFile {
                            parent_id: team.root,
                            name: "data.bin".into(),
                            on_conflict: OnConflict::Fail,
                        },
                        content_type: Some("application/octet-stream".into()),
                        chunking: SMALL_CHUNKING.id.to_string(),
                        size: len as u64,
                        content_hash: m.content_hash,
                        chunks: m.chunks.clone(),
                    },
                )
                .await
                .expect("create_upload");
            for (o, c) in with_offsets(&m.chunks) {
                let piece = bytes::Bytes::copy_from_slice(&data[o..o + c.size as usize]);
                s.transfers.put_chunk(&ctx, up.id, c.hash, piece).await.expect("put_chunk");
            }
            let w = s.transfers.complete_upload(&ctx, up.id).await.expect("complete_upload");
            let content = s
                .transfers
                .open_content(&ctx, w.value.0.id, None, None)
                .await
                .expect("open_content");
            assert_eq!(content.content_hash, Hash::of(&data));
            assert_eq!(content.total_size, len as u64);
            let mut body = content.body;
            let mut got = Vec::with_capacity(len);
            while let Some(b) = body.next().await {
                got.extend_from_slice(&b.expect("body"));
            }
            assert!(got == data, "the download differs from the upload");
            assert_eq!(Hash::of(&got), Hash::of(&data));
        });
    }
}
