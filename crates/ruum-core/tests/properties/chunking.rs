//! Chunking (design section 5.3; `blobs::chunk`).
//!
//! The model:
//!
//! - The chunks of data cover it exactly: in order, their sizes add up to the length, each
//!   chunk's hash is BLAKE3 of its bytes, every chunk but the last is between the minimum and
//!   the maximum size, and the last is at most the maximum. Joined again they give the same
//!   bytes and the same BLAKE3 as the whole.
//! - Boundaries are content-defined (what FastCDC guarantees, see
//!   `boundaries_are_stable_under_inserts`): after an insert, the chunks before it stay, and once
//!   the new chunking cuts where the old one cut (shifted), every later chunk is the old one,
//!   shifted. How soon that happens is not bounded; a test over fixed seeds checks that it is
//!   soon for most inserts.
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

/// The ends of the chunks: the cut points, and the length of the data last.
fn ends(data: &[u8]) -> Vec<usize> {
    with_offsets(&chunk(data, SMALL_CHUNKING))
        .iter()
        .map(|(o, c)| o + c.size as usize)
        .collect()
}

/// `old` with `insert` put in at `at`.
fn inserted(old: &[u8], at: usize, insert: &[u8]) -> Vec<u8> {
    let mut new = old[..at].to_vec();
    new.extend_from_slice(insert);
    new.extend_from_slice(&old[at..]);
    new
}

/// The resynchronisation point of an insert of `insert_len` bytes at `at`: the first end in the
/// new chunking, at or after the end of the insert, that is an old end shifted by `insert_len`.
/// The end of the data always is one.
fn resync_point(old_ends: &[usize], new_ends: &[usize], at: usize, insert_len: usize) -> usize {
    *new_ends
        .iter()
        .find(|b| **b >= at + insert_len && old_ends.binary_search(&(**b - insert_len)).is_ok())
        .expect("the end of the data is a common end")
}

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

    /// What FastCDC guarantees after an insert of `insert_len` bytes at `at`:
    ///
    /// - Before: a chunk that ends before `at` (strictly), and starts at least one maximum chunk
    ///   before the end of the data, keeps its offset and hash. FastCDC decides a cut at `L`
    ///   from the bytes of the chunk up to and including the byte at `L` (the first byte of the
    ///   next chunk), so a chunk that ends exactly at `at` may change; and near the end of the
    ///   data the cut depends on how much data is left.
    /// - After: from the resynchronisation point on (see [`resync_point`]), every old chunk is
    ///   in the new chunking with the same hash, shifted by `insert_len`. A cut sequence depends
    ///   only on the bytes from its start, so once both chunkings cut at the same byte of the
    ///   old data, they stay together.
    ///
    /// FastCDC does not bound how far after the insert the resynchronisation point is: the new
    /// chunks may keep cutting between the old cuts (a cut is never within the minimum size of
    /// the chunk start, the mask changes at the average size, and cuts at the maximum size are
    /// forced). `boundaries_resynchronise_soon_after_most_inserts` measures it.
    #[test]
    fn boundaries_are_stable_under_inserts(
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
        let new = inserted(&old, at, &insert);

        let old_chunks = with_offsets(&chunk(&old, p.clone()));
        let new_chunks = with_offsets(&chunk(&new, p));
        let in_new = |o: usize, c: &ChunkRef| new_chunks.iter().any(|(no, nc)| *no == o && nc == c);
        let old_ends: Vec<usize> = old_chunks.iter().map(|(o, c)| o + c.size as usize).collect();
        let new_ends: Vec<usize> = new_chunks.iter().map(|(o, c)| o + c.size as usize).collect();
        let resync = resync_point(&old_ends, &new_ends, at, insert_len) - insert_len;
        for (o, c) in &old_chunks {
            let end = o + c.size as usize;
            if end < at && o + max <= len {
                prop_assert!(in_new(*o, c), "the chunk at {} (before the insert at {}) moved", o, at);
            }
            if *o >= resync {
                prop_assert!(
                    in_new(o + insert_len, c),
                    "the chunk at {} (after the resynchronisation at {}) is not at {}",
                    o, resync, o + insert_len
                );
            }
        }
    }
}

/// How soon boundaries resynchronise in practice, over 200 fixed inserts (so the test is
/// deterministic): of the inserts with more than two maximum chunks (128 KiB) of data after
/// them, at least 90% resynchronise within 128 KiB after the end of the insert. When it was
/// written the count was 143 of 150. Fixed-size chunks would never resynchronise, so this also
/// keeps the property above from passing when chunking stops being content-defined.
#[test]
fn boundaries_resynchronise_soon_after_most_inserts() {
    let window = 2 * SMALL_CHUNKING.max_size as usize;
    let (mut eligible, mut soon) = (0, 0);
    for k in 0..200u64 {
        let seed = k.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x1234;
        let len = 200_000 + (seed % 700_000) as usize;
        let at = (seed >> 20) as usize % (len + 1);
        let insert_len = 1 + (seed >> 40) as usize % 4_999;
        if len - at <= window {
            continue;
        }
        let old = bytes(seed, len);
        let new = inserted(&old, at, &bytes(seed ^ 0x5555, insert_len));
        let r = resync_point(&ends(&old), &ends(&new), at, insert_len);
        eligible += 1;
        if r - (at + insert_len) <= window {
            soon += 1;
        }
    }
    assert!(
        soon * 10 >= eligible * 9,
        "only {soon} of {eligible} inserts resynchronised within {window} bytes"
    );
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
