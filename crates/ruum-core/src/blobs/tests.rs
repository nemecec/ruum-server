//! Tests of the blobs and versions areas on the in-memory content store: chunking, uploads,
//! downloads, `putContent`, the version policy, versions, diff, signed URLs, garbage collection,
//! permissions, and the undo of the version events.

use super::testing::World;
use super::*;
use crate::actor::{PrincipalKind, PrincipalRef};
use crate::authz::{Role, Scope};
use crate::error::ErrorCode;
use crate::ids::PrincipalId;
use crate::journal::{EventType, Journal};
use crate::paging::PageRequest;
use crate::versions::diff::DiffService;
use crate::versions::{PluginDiffService, VersionFilter, VersionStore};
use futures::StreamExt;
use std::sync::Arc;

fn code<T: std::fmt::Debug>(r: Result<T>) -> ErrorCode {
    match r {
        Err(Error::Api(e)) => e.code,
        other => panic!("expected an API error, got {other:?}"),
    }
}

async fn read(c: Content) -> Vec<u8> {
    let mut out = Vec::new();
    let mut body = c.body;
    while let Some(part) = body.next().await {
        out.extend_from_slice(&part.unwrap());
    }
    out
}

fn new_upload(target: UploadTarget, data: &[u8]) -> NewUpload {
    let params = chunking_for(data.len() as u64, Some("text/plain"));
    NewUpload {
        target,
        content_type: Some("text/plain".into()),
        chunking: params.id.to_string(),
        size: data.len() as u64,
        content_hash: Hash::of(data),
        chunks: chunk(data, params),
    }
}

/// Uploads `data` through the resumable protocol: create, send the missing chunks, complete.
async fn upload(
    store: &dyn Transfers,
    ctx: &Context,
    target: UploadTarget,
    data: &[u8],
) -> Result<crate::journal::Written<(Node, Version)>> {
    let req = new_upload(target, data);
    let up = store.create_upload(ctx, req.clone()).await?;
    let mut offset = 0usize;
    for c in &req.chunks {
        let part = Bytes::copy_from_slice(&data[offset..offset + c.size as usize]);
        offset += c.size as usize;
        if up.missing.contains(&c.hash) {
            store.put_chunk(ctx, up.id, c.hash, part).await?;
        }
    }
    store.complete_upload(ctx, up.id).await
}

fn new_file(w: &World, name: &str) -> UploadTarget {
    UploadTarget::NewFile {
        parent_id: w.docs,
        name: name.into(),
        on_conflict: crate::nodes::OnConflict::Fail,
    }
}

fn pseudo_random(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

// ---------------------------------------------------------------------------- chunking

#[test]
fn hashes_parse_only_lowercase_hex() {
    let h = Hash::of(b"abc");
    assert_eq!(Hash::from_hex(&h.to_hex()).unwrap(), h);
    assert!(Hash::from_hex(&h.to_hex().to_uppercase()).is_err());
    assert!(Hash::from_hex("abc").is_err());
}

#[test]
fn chunking_depends_on_size_and_type() {
    assert_eq!(chunking_for(10, None), SMALL_CHUNKING);
    assert_eq!(chunking_for(10 * 1024 * 1024, None), DEFAULT_CHUNKING);
    assert_eq!(
        chunking_for(10 * 1024 * 1024, Some("text/csv; charset=utf-8")),
        SMALL_CHUNKING
    );
    assert_eq!(
        chunking_for(100 * 1024 * 1024, Some("text/plain")),
        DEFAULT_CHUNKING
    );
    assert_eq!(
        chunking_by_id("fastcdc-2020-16k-32k-64k"),
        Some(SMALL_CHUNKING)
    );
    assert_eq!(chunking_by_id("other"), None);
}

#[test]
fn chunks_are_content_defined_and_inside_the_bounds() {
    let data = pseudo_random(1024 * 1024, 7);
    let a = chunk(&data, SMALL_CHUNKING);
    assert_eq!(a.iter().map(|c| c.size as usize).sum::<usize>(), data.len());
    for c in &a[..a.len() - 1] {
        assert!((SMALL_CHUNKING.min_size..=SMALL_CHUNKING.max_size).contains(&c.size));
    }
    // An insert near the start changes only the chunks around it.
    let mut edited = data.clone();
    edited.splice(1000..1000, b"inserted".iter().copied());
    let b = chunk(&edited, SMALL_CHUNKING);
    let same = b.iter().filter(|c| a.contains(c)).count();
    assert!(same >= a.len() - 3, "{same} of {}", a.len());
    assert!(chunk(b"", SMALL_CHUNKING).is_empty());
}

#[test]
fn manifests_round_trip() {
    let data = pseudo_random(200_000, 3);
    let m = manifest_of(&data, SMALL_CHUNKING);
    let bytes = m.encode();
    assert_eq!(
        Manifest::decode(ManifestFormat::ChunksV1, &bytes).unwrap(),
        m
    );
    assert!(Manifest::decode(ManifestFormat::ChunksV1, &bytes[..bytes.len() - 1]).is_err());
    let empty = manifest_of(b"", SMALL_CHUNKING);
    assert_eq!(
        Manifest::decode(ManifestFormat::ChunksV1, &empty.encode()).unwrap(),
        empty
    );
}

// ---------------------------------------------------------------------------- uploads

#[tokio::test]
async fn an_upload_makes_a_file_and_a_version() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let data = pseudo_random(300_000, 1);
    let r = upload(&*w.store, &ctx, new_file(&w, "a.bin"), &data)
        .await
        .unwrap();
    let (node, v) = r.value;
    assert_eq!(node.head_version, Some(v.id));
    assert_eq!(node.size_bytes, data.len() as i64);
    assert_eq!(v.content_hash, Hash::of(&data));
    let types: Vec<_> = w
        .journal
        .entries(w.tenant)
        .iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(
        types,
        vec![EventType::NodeCreated, EventType::VersionCreated]
    );
    let entry = w.journal.entries(w.tenant).pop().unwrap();
    assert_eq!(entry.seq, r.change.seq.unwrap());
    assert_eq!(entry.revision, Some(node.revision));
    assert_eq!(entry.data["coalesced"], false);
    assert!(entry.data["previous_version_id"].is_null());

    let all = w
        .store
        .open_content(&ctx, node.id, None, None)
        .await
        .unwrap();
    assert_eq!(all.total_size, data.len() as u64);
    assert_eq!(read(all).await, data);
    let part = w
        .store
        .open_content(
            &ctx,
            node.id,
            Some(v.id),
            Some(ByteRange {
                start: 70_000,
                end: 140_000,
            }),
        )
        .await
        .unwrap();
    assert_eq!(read(part).await, data[70_000..=140_000]);
    assert_eq!(
        code(
            w.store
                .open_content(
                    &ctx,
                    node.id,
                    None,
                    Some(ByteRange {
                        start: 300_000,
                        end: 300_001
                    })
                )
                .await
        ),
        ErrorCode::RangeNotSatisfiable
    );
}

#[tokio::test]
async fn the_server_asks_only_for_missing_chunks() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let data = pseudo_random(200_000, 2);
    upload(&*w.store, &ctx, new_file(&w, "a.bin"), &data)
        .await
        .unwrap();
    let up = w
        .store
        .create_upload(&ctx, new_upload(new_file(&w, "b.bin"), &data))
        .await
        .unwrap();
    assert!(up.missing.is_empty());
    let mut changed = data.clone();
    changed.extend_from_slice(b"tail");
    let req = new_upload(new_file(&w, "c.bin"), &changed);
    let up = w.store.create_upload(&ctx, req.clone()).await.unwrap();
    assert!(!up.missing.is_empty());
    assert!(up.missing.len() < req.chunks.len());
    // Resume: get_upload says the same.
    let again = w.store.get_upload(&ctx, up.id).await.unwrap();
    assert_eq!(again.missing, up.missing);
}

#[tokio::test]
async fn bad_chunks_and_incomplete_uploads_fail() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let data = b"hello world".to_vec();
    let req = new_upload(new_file(&w, "a.txt"), &data);
    let up = w.store.create_upload(&ctx, req.clone()).await.unwrap();
    let h = req.chunks[0].hash;
    assert_eq!(
        code(
            w.store
                .put_chunk(&ctx, up.id, h, Bytes::from_static(b"other"))
                .await
        ),
        ErrorCode::HashMismatch
    );
    assert_eq!(
        code(
            w.store
                .put_chunk(&ctx, up.id, Hash::of(b"x"), Bytes::from_static(b"x"))
                .await
        ),
        ErrorCode::ValidationFailed
    );
    assert_eq!(
        code(w.store.complete_upload(&ctx, up.id).await),
        ErrorCode::ChunkMissing
    );
    // A wrong content hash is found at completion.
    let mut wrong = req.clone();
    wrong.content_hash = Hash::of(b"something else");
    let up = w.store.create_upload(&ctx, wrong).await.unwrap();
    w.store
        .put_chunk(&ctx, up.id, h, Bytes::from(data.clone()))
        .await
        .unwrap();
    assert_eq!(
        code(w.store.complete_upload(&ctx, up.id).await),
        ErrorCode::HashMismatch
    );
    // Unknown chunking, sizes that do not add up.
    let mut bad = req.clone();
    bad.chunking = "fastcdc-2020-1k".into();
    assert_eq!(
        code(w.store.create_upload(&ctx, bad).await),
        ErrorCode::ValidationFailed
    );
    let mut bad = req.clone();
    bad.size += 1;
    assert_eq!(
        code(w.store.create_upload(&ctx, bad).await),
        ErrorCode::ValidationFailed
    );
}

#[tokio::test]
async fn uploads_belong_to_the_actor_that_started_them() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let other = w.user_with(Role::Manage, w.root).await;
    let up = w
        .store
        .create_upload(&ctx, new_upload(new_file(&w, "a.txt"), b"abc"))
        .await
        .unwrap();
    assert_eq!(
        code(w.store.get_upload(&other, up.id).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(w.store.complete_upload(&other, up.id).await),
        ErrorCode::NotFound
    );
    w.store.abort_upload(&ctx, up.id).await.unwrap();
    let up = w.store.get_upload(&ctx, up.id).await.unwrap();
    assert_eq!(up.status, UploadStatus::Aborted);
    assert_eq!(
        code(w.store.complete_upload(&ctx, up.id).await),
        ErrorCode::Conflict
    );
}

#[tokio::test]
async fn an_empty_file_and_a_new_version_with_base() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let (node, v1) = upload(&*w.store, &ctx, new_file(&w, "e.txt"), b"")
        .await
        .unwrap()
        .value;
    assert_eq!(v1.size_bytes, 0);
    assert!(
        read(
            w.store
                .open_content(&ctx, node.id, None, None)
                .await
                .unwrap()
        )
        .await
        .is_empty()
    );
    let other = w.user_with(Role::Edit, w.docs).await;
    let target = |base| UploadTarget::NewVersion {
        node_id: node.id,
        base_version: base,
    };
    let (_, v2) = upload(&*w.store, &other, target(Some(v1.id)), b"two")
        .await
        .unwrap()
        .value;
    assert_ne!(v2.id, v1.id);
    assert_eq!(
        code(upload(&*w.store, &ctx, target(Some(v1.id)), b"three").await),
        ErrorCode::VersionMismatch
    );
    // The name is taken for a second new file.
    assert_eq!(
        code(upload(&*w.store, &ctx, new_file(&w, "E.TXT"), b"x").await),
        ErrorCode::NameTaken
    );
}

// ---------------------------------------------------------------------------- putContent and policy

#[tokio::test]
async fn saves_by_the_same_actor_coalesce() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let (node, v1) = upload(&*w.store, &ctx, new_file(&w, "n.txt"), b"one")
        .await
        .unwrap()
        .value;
    let put = |c: Context, data: &'static [u8], if_v: Option<VersionId>| {
        let store = w.store.clone();
        async move {
            store
                .put_content(&c, node.id, Bytes::from_static(data), None, if_v)
                .await
        }
    };
    let r = put(ctx.clone(), b"two", Some(v1.id)).await.unwrap();
    assert_eq!(r.value.1.id, v1.id, "same actor inside the window: merged");
    let e = w.journal.entries(w.tenant).pop().unwrap();
    assert_eq!(e.data["coalesced"], true);
    assert_eq!(e.data["version_id"], serde_json::json!(v1.id));
    assert_eq!(
        read(
            w.store
                .open_content(&ctx, node.id, None, None)
                .await
                .unwrap()
        )
        .await,
        b"two"
    );
    let other = w.user_with(Role::Edit, w.docs).await;
    let r2 = put(other.clone(), b"three", None).await.unwrap();
    assert_ne!(r2.value.1.id, v1.id);
    assert_eq!(r2.value.0.head_version, Some(r2.value.1.id));
    assert_eq!(
        code(put(ctx.clone(), b"four", Some(v1.id)).await),
        ErrorCode::PreconditionFailed
    );
    // A named head never takes later saves.
    w.store
        .set_label(&other, node.id, r2.value.1.id, Some("Final".into()))
        .await
        .unwrap();
    let r3 = put(other.clone(), b"five", None).await.unwrap();
    assert_ne!(r3.value.1.id, r2.value.1.id);
}

#[tokio::test]
async fn retention_and_thinning_keep_named_versions_and_the_head() {
    let w = World::new().await;
    let mut ids = Vec::new();
    let mut node = None;
    for i in 0..5u8 {
        // Different users, so nothing coalesces.
        let c = w.user_with(Role::Edit, w.docs).await;
        let target = match node {
            None => new_file(&w, "r.txt"),
            Some(n) => UploadTarget::NewVersion {
                node_id: n,
                base_version: None,
            },
        };
        let (n, v) = upload(&*w.store, &c, target, &[b'a' + i])
            .await
            .unwrap()
            .value;
        node = Some(n.id);
        ids.push(v.id);
    }
    let node = node.unwrap();
    let ctx = w.owner_ctx();
    w.store
        .set_label(&ctx, node, ids[0], Some("First".into()))
        .await
        .unwrap();
    let policy = crate::versions::VersionPolicy {
        keep_count: Some(2),
        ..Default::default()
    };
    let gone = w.store.apply_policy(node, &policy, crate::now());
    assert_eq!(gone.len(), 2, "{gone:?}");
    let left: Vec<_> = w
        .store
        .list(&ctx, node, PageRequest::default())
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|v| v.id)
        .collect();
    assert_eq!(left, vec![ids[4], ids[3], ids[0]]);
}

// ---------------------------------------------------------------------------- versions

#[tokio::test]
async fn versions_list_get_restore_and_label() {
    let w = World::new().await;
    let a = w.owner_ctx();
    let b = w.user_with(Role::Edit, w.docs).await;
    let (node, v1) = upload(&*w.store, &a, new_file(&w, "v.txt"), b"first\n")
        .await
        .unwrap()
        .value;
    let v2 = w
        .store
        .put_content(&b, node.id, Bytes::from_static(b"second\n"), None, None)
        .await
        .unwrap()
        .value
        .1;
    let r = w.store.restore(&a, node.id, v1.id).await.unwrap();
    let v3 = r.value;
    assert_eq!(v3.restored_from, Some(v1.id));
    assert_eq!(v3.content_hash, v1.content_hash);
    let e = w.journal.entries(w.tenant).pop().unwrap();
    assert_eq!(e.event, EventType::VersionRestored);
    assert_eq!(e.data["previous_version_id"], serde_json::json!(v2.id));
    assert_eq!(
        read(w.store.open_content(&a, node.id, None, None).await.unwrap()).await,
        b"first\n"
    );

    let page = w
        .store
        .list(
            &a,
            node.id,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.items.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![v3.id, v2.id]
    );
    let rest = w
        .store
        .list(
            &a,
            node.id,
            PageRequest {
                cursor: page.next_cursor,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        rest.items.iter().map(|v| v.id).collect::<Vec<_>>(),
        vec![v1.id]
    );
    assert!(rest.next_cursor.is_none());

    let l = w
        .store
        .set_label(&a, node.id, v1.id, Some(" Before import ".into()))
        .await
        .unwrap();
    assert_eq!(l.value.label.as_deref(), Some("Before import"));
    let e = w.journal.entries(w.tenant).pop().unwrap();
    assert_eq!(e.event, EventType::VersionLabeled);
    assert!(e.data["old_label"].is_null());
    let named = w
        .store
        .list_filtered(
            &a,
            node.id,
            VersionFilter {
                labeled: Some(true),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(named.items.len(), 1);
    assert_eq!(
        code(
            w.store
                .set_label(&a, node.id, v1.id, Some("x".repeat(201)))
                .await
        ),
        ErrorCode::ValidationFailed
    );
    assert_eq!(
        code(w.store.get(&a, node.id, VersionId::new()).await),
        ErrorCode::NotFound
    );
    assert_eq!(w.store.get(&a, node.id, v2.id).await.unwrap().id, v2.id);
}

// ---------------------------------------------------------------------------- permissions

#[tokio::test]
async fn permissions_follow_the_roles_and_scopes() {
    let w = World::new().await;
    let owner = w.owner_ctx();
    let (node, v1) = upload(&*w.store, &owner, new_file(&w, "p.txt"), b"secret")
        .await
        .unwrap()
        .value;
    let viewer = w.user_with(Role::View, w.docs).await;
    let stranger = w.ctx(crate::actor::Actor::user(crate::ids::UserId::new()));
    assert!(
        w.store
            .open_content(&viewer, node.id, None, None)
            .await
            .is_ok()
    );
    assert!(w.store.get(&viewer, node.id, v1.id).await.is_ok());
    assert_eq!(
        code(
            w.store
                .put_content(&viewer, node.id, Bytes::from_static(b"x"), None, None)
                .await
        ),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(w.store.restore(&viewer, node.id, v1.id).await),
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        code(w.store.open_content(&stranger, node.id, None, None).await),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(
            w.store
                .create_upload(&viewer, new_upload(new_file(&w, "q.txt"), b"q"))
                .await
        ),
        ErrorCode::PermissionDenied
    );
    // An agent of the owner with a view scope: reads yes, writes `scope_exceeded`.
    let agent = w.agent_ctx(Some(w.docs), Role::View);
    assert!(
        w.store
            .open_content(&agent, node.id, None, None)
            .await
            .is_ok()
    );
    assert_eq!(
        code(
            w.store
                .put_content(&agent, node.id, Bytes::from_static(b"x"), None, None)
                .await
        ),
        ErrorCode::ScopeExceeded
    );
    // An agent scoped elsewhere does not see the file.
    let outside = w.agent_ctx(Some(NodeId::new()), Role::Manage);
    assert_eq!(
        code(w.store.open_content(&outside, node.id, None, None).await),
        ErrorCode::NotFound
    );
    // A view share link cannot write at all.
    let link = Context {
        actor: crate::actor::Actor {
            actor: PrincipalRef {
                kind: PrincipalKind::ShareLink,
                id: PrincipalId::new(),
            },
            on_behalf_of: None,
        },
        scope: Some(Scope {
            subtree: Some(w.docs),
            max_role: Role::View,
            expires_at: None,
        }),
        ..w.owner_ctx()
    };
    assert_eq!(
        code(
            w.store
                .put_content(&link, node.id, Bytes::from_static(b"x"), None, None)
                .await
        ),
        ErrorCode::UserRequired
    );
    // Another tenant does not see the node, even with the right user.
    let mut other_tenant = w.owner_ctx();
    other_tenant.tenant = crate::ids::TenantId::new();
    assert_eq!(
        code(
            w.store
                .open_content(&other_tenant, node.id, None, None)
                .await
        ),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(
            w.store
                .list(&other_tenant, node.id, PageRequest::default())
                .await
        ),
        ErrorCode::NotFound
    );
}

// ---------------------------------------------------------------------------- diff, grants, gc

#[tokio::test]
async fn diffs_use_the_best_plugin() {
    let w = World::new().await;
    let a = w.owner_ctx();
    let b = w.user_with(Role::Edit, w.docs).await;
    let target = UploadTarget::NewFile {
        parent_id: w.docs,
        name: "c.json".into(),
        on_conflict: crate::nodes::OnConflict::Fail,
    };
    let (node, v1) = upload(&*w.store, &a, target, br#"{"a": 1, "b": [1]}"#)
        .await
        .unwrap()
        .value;
    let v2 = w
        .store
        .put_content(
            &b,
            node.id,
            Bytes::from_static(br#"{"b": [1, 2], "a": 2}"#),
            Some("application/json".into()),
            None,
        )
        .await
        .unwrap()
        .value
        .1;
    let diffs = PluginDiffService::new(w.nodes.clone(), w.store.clone(), w.store.clone());
    let plugins = diffs.plugins(&a, node.id).await.unwrap();
    assert_eq!(plugins[0].name, "json");
    assert_eq!(plugins.last().unwrap().name, "binary");
    let r = diffs.diff(&a, node.id, v1.id, v2.id, None).await.unwrap();
    assert_eq!(r.plugin, "json@1.0.0");
    assert_eq!(r.from, Some(v1.id));
    assert_eq!(r.summary.modified, 1);
    assert_eq!(r.summary.inserted, 1);
    let t = diffs
        .diff(&a, node.id, v1.id, v2.id, Some("text".into()))
        .await
        .unwrap();
    assert_eq!(t.plugin, "text@1.0.0");
    assert_eq!(
        code(
            diffs
                .diff(&a, node.id, v1.id, v2.id, Some("nope".into()))
                .await
        ),
        ErrorCode::ValidationFailed
    );
    let stranger = w.ctx(crate::actor::Actor::user(crate::ids::UserId::new()));
    assert_eq!(
        code(diffs.diff(&stranger, node.id, v1.id, v2.id, None).await),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn signed_grants_open_one_version() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    let (node, v1) = upload(&*w.store, &ctx, new_file(&w, "g.txt"), b"granted")
        .await
        .unwrap()
        .value;
    let (token, claims) = w
        .store
        .content_grant(&ctx, node.id, None, Disposition::Inline)
        .await
        .unwrap();
    assert_eq!(claims.version, v1.id);
    assert_eq!(claims.name, "g.txt");
    let (c, _) = w.store.open_granted(&token, None).await.unwrap();
    assert_eq!(read(c).await, b"granted");
    assert_eq!(
        code(w.store.open_granted(&format!("{token}x"), None).await),
        ErrorCode::NotFound
    );
    let stranger = w.ctx(crate::actor::Actor::user(crate::ids::UserId::new()));
    assert_eq!(
        code(
            w.store
                .content_grant(&stranger, node.id, None, Disposition::Attachment)
                .await
        ),
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn garbage_collection_keeps_live_chunks() {
    let w = World::new().await;
    let ctx = w.owner_ctx();
    upload(&*w.store, &ctx, new_file(&w, "keep.txt"), b"keep me")
        .await
        .unwrap();
    let req = new_upload(new_file(&w, "drop.txt"), b"drop me");
    let up = w.store.create_upload(&ctx, req.clone()).await.unwrap();
    w.store
        .put_chunk(
            &ctx,
            up.id,
            req.chunks[0].hash,
            Bytes::from_static(b"drop me"),
        )
        .await
        .unwrap();
    assert_eq!(w.blobs.count(w.tenant), 2);
    // The open upload keeps its chunk.
    let r = w
        .store
        .collect_garbage(w.tenant, time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(r.deleted, 0);
    w.store.abort_upload(&ctx, up.id).await.unwrap();
    // Inside the grace period nothing goes.
    let r = w
        .store
        .collect_garbage(w.tenant, time::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(r.deleted, 0);
    let r = w
        .store
        .collect_garbage(w.tenant, time::Duration::ZERO)
        .await
        .unwrap();
    assert_eq!(
        r,
        gc::GcReport {
            live: 1,
            deleted: 1
        }
    );
    assert_eq!(w.blobs.count(w.tenant), 1);
}

// ---------------------------------------------------------------------------- undo

/// The services of the world, for `agents::undo::invert`.
fn services(w: &World) -> crate::Services {
    let mut s = crate::Services::in_memory();
    s.nodes = w.nodes.clone();
    s.transfers = w.store.clone();
    s.versions = w.store.clone();
    s.journal = w.journal.clone();
    s.diffs = Arc::new(PluginDiffService::new(
        w.nodes.clone(),
        w.store.clone(),
        w.store.clone(),
    ));
    s
}

#[tokio::test]
async fn undo_inverts_the_version_events() {
    let w = World::new().await;
    let s = services(&w);
    let owner = w.owner_ctx();
    let agent = w.agent_ctx(None, Role::Edit);
    let (node, v1) = upload(&*w.store, &owner, new_file(&w, "u.txt"), b"mine\n")
        .await
        .unwrap()
        .value;
    let last = |w: &World| w.journal.entries(w.tenant).pop().unwrap();

    // version.created by the agent → the owner's undo restores v1.
    w.store
        .put_content(&agent, node.id, Bytes::from_static(b"agent\n"), None, None)
        .await
        .unwrap();
    let e = last(&w);
    let change = crate::agents::undo::invert(&s, &owner, &e).await.unwrap();
    assert!(change.is_some());
    let head = w
        .store
        .list(&owner, node.id, PageRequest::default())
        .await
        .unwrap()
        .items[0]
        .clone();
    assert_eq!(head.restored_from, Some(v1.id));
    assert_eq!(
        read(
            w.store
                .open_content(&owner, node.id, None, None)
                .await
                .unwrap()
        )
        .await,
        b"mine\n"
    );

    // version.restored by the agent → back to the version before the restore.
    let before = head.id;
    w.store.restore(&agent, node.id, v1.id).await.unwrap();
    let e = last(&w);
    assert_eq!(e.event, EventType::VersionRestored);
    crate::agents::undo::invert(&s, &owner, &e).await.unwrap();
    let head = w
        .store
        .list(&owner, node.id, PageRequest::default())
        .await
        .unwrap()
        .items[0]
        .clone();
    assert_eq!(head.restored_from, Some(before));

    // version.labeled by the agent → the old label comes back.
    w.store
        .set_label(&owner, node.id, v1.id, Some("Mine".into()))
        .await
        .unwrap();
    w.store
        .set_label(&agent, node.id, v1.id, Some("Agent".into()))
        .await
        .unwrap();
    let e = last(&w);
    crate::agents::undo::invert(&s, &owner, &e).await.unwrap();
    assert_eq!(
        w.store
            .get(&owner, node.id, v1.id)
            .await
            .unwrap()
            .label
            .as_deref(),
        Some("Mine")
    );

    // The first version of a file has nothing before it: no inverse.
    let first = w
        .journal
        .entries(w.tenant)
        .into_iter()
        .find(|e| e.event == EventType::VersionCreated)
        .unwrap();
    assert!(
        crate::agents::undo::invert(&s, &owner, &first)
            .await
            .unwrap()
            .is_none()
    );

    // A viewer cannot undo (the inverse needs edit).
    let viewer = w.user_with(Role::View, w.docs).await;
    assert_eq!(
        code(crate::agents::undo::invert(&s, &viewer, &e).await),
        ErrorCode::PermissionDenied
    );
    assert!(w.journal.latest_seq(&owner).await.unwrap() > 0);
}
