//! The conformance suite of [`AppStore`]: the memory store and the Postgres store pass the same
//! cases (`tests` below runs both; the Postgres run needs `DATABASE_URL`).

use super::*;
use crate::actor::Context;
use crate::ids::UserId;

/// What a case needs from a backend.
#[async_trait]
pub(crate) trait Harness: Send + Sync {
    fn store(&self) -> &dyn AppStore;
    /// A new tenant and a user of it.
    async fn tenant(&self) -> (TenantId, UserId);
    /// A new web app node in the tenant (the typed node rows that `apps` refers to).
    async fn app_node(&self, tenant: TenantId, user: UserId) -> NodeId;
}

fn ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

fn app(node: NodeId, slug: &str) -> NewApp {
    NewApp {
        node_id: node,
        slug: slug.into(),
        settings: AppSettings::default(),
    }
}

fn deployment(app: NodeId, env: Environment, state: DeploymentState) -> NewDeployment {
    NewDeployment {
        id: Uuid::now_v7(),
        app_node_id: app,
        environment: env,
        state,
        short_id: super::super::slugs::short_id(),
        label: Some("plant list".into()),
        draft: None,
        source: DeploymentSource {
            kind: "file".into(),
            node_id: Some(NodeId::new()),
            version_id: Some(VersionId::new()),
            files: None,
        },
        archive: Archive {
            content_hash: [7; 32],
            size: 10,
            chunks: vec![1, 2, 3],
        },
        manifest: Some(serde_json::json!({ "runtime": "static" })),
        stages: serde_json::json!([]),
        published_from: None,
        new_chunks: Vec::new(),
        expires_at: None,
    }
}

/// A unique slug per run (the Postgres runs of one database share the global indexes).
fn unique(base: &str) -> String {
    format!("{base}-{}", &Uuid::now_v7().simple().to_string()[20..])
}

/// Tenant slugs are unique across tenants and made once; approvals start off.
pub(crate) async fn tenant_slugs_span_tenants(h: &dyn Harness) {
    let s = h.store();
    let (a, _) = h.tenant().await;
    let (b, _) = h.tenant().await;
    let garden = unique("garden");
    let other = unique("garden");
    assert!(s.tenant_settings(a).await.unwrap().is_none());
    let ta = s
        .ensure_tenant_settings(a, std::slice::from_ref(&garden))
        .await
        .unwrap();
    assert_eq!(ta.slug, garden);
    assert!(!ta.public_apps_approved && !ta.custom_domains_approved);
    // B cannot take A's slug; it gets the next candidate.
    let tb = s
        .ensure_tenant_settings(b, &[garden.clone(), other.clone()])
        .await
        .unwrap();
    assert_eq!(tb.slug, other);
    // Made once: a second call keeps the slug.
    let again = s.ensure_tenant_settings(a, &[unique("x")]).await.unwrap();
    assert_eq!(again.slug, garden);
    // Every candidate taken: conflict, nothing made.
    let (c, _) = h.tenant().await;
    let e = s
        .ensure_tenant_settings(c, &[garden.clone(), other.clone()])
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 409);
    assert!(s.tenant_settings(c).await.unwrap().is_none());
    // Approvals: per tenant.
    let t = s
        .set_approval(
            a,
            &[],
            ApprovalChange {
                public_apps: Some(true),
                custom_domains: Some(true),
                by: "admin@ruum.test".into(),
            },
        )
        .await
        .unwrap();
    assert!(t.public_apps_approved && t.custom_domains_approved);
    assert!(
        !s.tenant_settings(b)
            .await
            .unwrap()
            .unwrap()
            .public_apps_approved
    );
    let t = s
        .set_approval(
            a,
            &[],
            ApprovalChange {
                public_apps: Some(false),
                custom_domains: None,
                by: "admin@ruum.test".into(),
            },
        )
        .await
        .unwrap();
    assert!(!t.public_apps_approved && t.custom_domains_approved);
}

/// App rows belong to their tenant: another tenant never reads, changes or removes them, and has
/// its own slug space.
pub(crate) async fn apps_stay_in_their_tenant(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let (b, ub) = h.tenant().await;
    let na = h.app_node(a, ua).await;
    let nb = h.app_node(b, ub).await;
    let row = s
        .insert_app(&ctx(a, ua), app(na, "planner"))
        .await
        .unwrap()
        .expect("the slug is free");
    assert_eq!(
        (row.tenant_id, row.node_id, row.slug.as_str()),
        (a, na, "planner")
    );
    assert_eq!(row.settings, AppSettings::default());
    assert!(row.live_deployment_id.is_none() && row.slug_fixed_at.is_none());
    assert_eq!(s.get_app(a, na).await.unwrap().unwrap(), row);
    assert!(s.get_app(b, na).await.unwrap().is_none());
    // The app slug space is per tenant: B has its own `planner`.
    assert_eq!(
        s.slug_state(b, "planner", nb).await.unwrap(),
        SlugState::Free
    );
    assert!(
        s.insert_app(&ctx(b, ub), app(nb, "planner"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(s.count_apps(a).await.unwrap(), 1);
    assert_eq!(s.count_apps(b).await.unwrap(), 1);
    // B cannot change A's settings.
    let changed = AppSettings {
        raw_copy_days: 7,
        ..AppSettings::default()
    };
    let data = settings_changed_data(&["raw_copy_days"], &changed);
    let e = s
        .update_settings(
            &ctx(b, ub),
            na,
            &AppSettings::default(),
            &changed,
            data.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 404);
    assert_eq!(
        s.get_app(a, na)
            .await
            .unwrap()
            .unwrap()
            .settings
            .raw_copy_days,
        0
    );
    let (after, change) = s
        .update_settings(
            &ctx(a, ua),
            na,
            &AppSettings::default(),
            &changed,
            data.clone(),
        )
        .await
        .unwrap()
        .expect("the stored settings are the expected ones");
    assert_eq!(after.settings.raw_copy_days, 7);
    assert!(change.seq.is_some(), "the change has a journal entry");
    assert_eq!(change.actor, Actor::user(ua));
    // A write based on an old copy (the defaults) does not overwrite the newer settings.
    let stale = AppSettings {
        telemetry_mode: crate::apps::settings::TelemetryMode::Full,
        ..AppSettings::default()
    };
    assert!(
        s.update_settings(&ctx(a, ua), na, &AppSettings::default(), &stale, data)
            .await
            .unwrap()
            .is_none(),
        "a compare-and-swap with an old copy writes nothing"
    );
    assert_eq!(s.get_app(a, na).await.unwrap().unwrap().settings, changed);
    // B's removal of A's node changes nothing.
    s.remove_app(b, na).await.unwrap();
    assert!(s.get_app(a, na).await.unwrap().is_some());
    // A deployment of A is not reachable from B.
    let d = s
        .insert_deployment(
            &ctx(a, ua),
            deployment(na, Environment::Preview, DeploymentState::Ready),
        )
        .await
        .unwrap();
    assert!(s.get_deployment(b, na, d.id).await.unwrap().is_none());
    assert!(s.get_deployment(b, nb, d.id).await.unwrap().is_none());
    assert!(
        s.list_deployments(b, na, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        !s.set_deployment_state(b, d.id, DeploymentState::Failed)
            .await
            .unwrap()
    );
    assert_eq!(
        s.get_deployment(a, na, d.id).await.unwrap().unwrap().state,
        DeploymentState::Ready
    );
    // B cannot deploy to A's app.
    let e = s
        .insert_deployment(
            &ctx(b, ub),
            deployment(na, Environment::Preview, DeploymentState::Pending),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 404);
}

/// A slug given once is never given to another app of the tenant, also after that app is gone;
/// the same app node gets it back.
pub(crate) async fn used_slugs_are_never_given_again(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let x = h.app_node(a, ua).await;
    let y = h.app_node(a, ua).await;
    s.insert_app(&ctx(a, ua), app(x, "planner"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        s.slug_state(a, "planner", y).await.unwrap(),
        SlugState::InUse(x)
    );
    assert_eq!(
        s.slug_state(a, "planner", x).await.unwrap(),
        SlugState::Free
    );
    // Taken now: nothing is written.
    assert!(
        s.insert_app(&ctx(a, ua), app(y, "planner"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(s.get_app(a, y).await.unwrap().is_none());
    // X is purged: its slug stays used.
    s.insert_deployment(
        &ctx(a, ua),
        deployment(x, Environment::Live, DeploymentState::Live),
    )
    .await
    .unwrap();
    s.remove_app(a, x).await.unwrap();
    assert!(s.get_app(a, x).await.unwrap().is_none());
    assert!(
        s.list_deployments(a, x, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(
        s.slug_state(a, "planner", y).await.unwrap(),
        SlugState::Retired
    );
    assert!(
        s.insert_app(&ctx(a, ua), app(y, "planner"))
            .await
            .unwrap()
            .is_none()
    );
    // The same node may have it again (a restore).
    assert_eq!(
        s.slug_state(a, "planner", x).await.unwrap(),
        SlugState::Free
    );
    // Another slug is fine.
    assert!(
        s.insert_app(&ctx(a, ua), app(y, "planner-2"))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(s.count_apps(a).await.unwrap(), 1);
}

/// Deployments: newest first, the environment filter, paging, and a release that becomes the
/// live deployment with a version and fixes the slug.
pub(crate) async fn deployments_list_and_releases(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let n = h.app_node(a, ua).await;
    let c = ctx(a, ua);
    s.insert_app(&c, app(n, "planner")).await.unwrap().unwrap();
    let p1 = s
        .insert_deployment(
            &c,
            deployment(n, Environment::Preview, DeploymentState::Pending),
        )
        .await
        .unwrap();
    assert!(p1.version_id.is_none() && p1.published_at.is_none());
    assert_eq!(p1.created_by, Actor::user(ua));
    let l1 = s
        .insert_deployment(&c, deployment(n, Environment::Live, DeploymentState::Live))
        .await
        .unwrap();
    assert!(l1.version_id.is_some() && l1.published_at.is_some());
    let row = s.get_app(a, n).await.unwrap().unwrap();
    assert_eq!(row.live_deployment_id, Some(l1.id));
    assert!(row.slug_fixed_at.is_some());
    let l2 = s
        .insert_deployment(&c, deployment(n, Environment::Live, DeploymentState::Live))
        .await
        .unwrap();
    assert_ne!(l1.version_id, l2.version_id);
    assert_eq!(
        s.get_app(a, n).await.unwrap().unwrap().live_deployment_id,
        Some(l2.id)
    );
    assert_eq!(
        s.get_deployment(a, n, l1.id).await.unwrap().unwrap().state,
        DeploymentState::Superseded
    );
    let all = s
        .list_deployments(a, n, None, PageRequest::default())
        .await
        .unwrap();
    let ids: Vec<Uuid> = all.items.iter().map(|d| d.id).collect();
    assert_eq!(ids, [l2.id, l1.id, p1.id]);
    assert!(all.next_cursor.is_none());
    let live = s
        .list_deployments(a, n, Some(Environment::Live), PageRequest::default())
        .await
        .unwrap();
    assert_eq!(live.items.len(), 2);
    let first = s
        .list_deployments(
            a,
            n,
            None,
            PageRequest {
                cursor: None,
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    let rest = s
        .list_deployments(
            a,
            n,
            None,
            PageRequest {
                cursor: first.next_cursor.clone(),
                limit: Some(2),
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.items.iter().map(|d| d.id).collect::<Vec<_>>(), [p1.id]);
    assert!(rest.next_cursor.is_none());
    // A deployment is read under its own app only.
    let other = h.app_node(a, ua).await;
    assert!(s.get_deployment(a, other, p1.id).await.unwrap().is_none());
    // The state changes.
    assert!(
        s.set_deployment_state(a, p1.id, DeploymentState::Running)
            .await
            .unwrap()
    );
    let got = s.get_deployment(a, n, p1.id).await.unwrap().unwrap();
    assert_eq!(got.state, DeploymentState::Running);
    assert_eq!(got.source, p1.source);
    assert_eq!(got.archive, p1.archive);
    assert_eq!(got.manifest, p1.manifest);
    // A bad cursor is a bad request.
    let e = s
        .list_deployments(
            a,
            n,
            None,
            PageRequest {
                cursor: Some(Cursor("nope".into())),
                limit: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 400);
}

/// A preview's stages, its finish (with draft supersession), a failure, releases with the
/// compare of the live deployment, versions and their deployments, approvals and the expiry
/// of previews (card W-2s).
pub(crate) async fn previews_releases_and_versions(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let n = h.app_node(a, ua).await;
    let c = ctx(a, ua);
    s.insert_app(&c, app(n, &unique("planner")))
        .await
        .unwrap()
        .unwrap();
    // Two previews of one draft: the older ready one is superseded by the newer one.
    let mut d1 = deployment(n, Environment::Preview, DeploymentState::Running);
    d1.draft = Some("sortable".into());
    let p1 = s.insert_deployment(&c, d1).await.unwrap();
    let stages = serde_json::json!([{ "name": "image", "state": "running" }]);
    assert!(s.update_stages(a, p1.id, &stages).await.unwrap());
    let check = serde_json::json!({ "passed": true });
    assert_eq!(
        s.finish_preview(&c, p1.id, &stages, Some(&check))
            .await
            .unwrap(),
        DeploymentState::Ready
    );
    let got = s.get_deployment(a, n, p1.id).await.unwrap().unwrap();
    assert_eq!(
        (got.state, got.stages.clone()),
        (DeploymentState::Ready, stages.clone())
    );
    assert_eq!(got.check_result, Some(check.clone()));
    // A finished preview takes no more stages.
    assert!(!s.update_stages(a, p1.id, &stages).await.unwrap());
    let mut d2 = deployment(n, Environment::Preview, DeploymentState::Running);
    d2.draft = Some("sortable".into());
    let p2 = s.insert_deployment(&c, d2).await.unwrap();
    let mut d3 = deployment(n, Environment::Preview, DeploymentState::Running);
    d3.draft = Some("colours".into());
    let p3 = s.insert_deployment(&c, d3).await.unwrap();
    s.finish_preview(&c, p2.id, &stages, None).await.unwrap();
    s.finish_preview(&c, p3.id, &stages, None).await.unwrap();
    let state = |id| async move { s.get_deployment(a, n, id).await.unwrap().unwrap().state };
    assert_eq!(state(p1.id).await, DeploymentState::Superseded);
    assert_eq!(state(p2.id).await, DeploymentState::Ready);
    assert_eq!(state(p3.id).await, DeploymentState::Ready);
    // A failure.
    let p4 = s
        .insert_deployment(
            &c,
            deployment(n, Environment::Preview, DeploymentState::Running),
        )
        .await
        .unwrap();
    s.fail_deployment(&c, p4.id, &stages, None, "image", "image_failed")
        .await
        .unwrap();
    assert_eq!(state(p4.id).await, DeploymentState::Failed);
    // The first release: the live deployment must still be none.
    let mut r1 = deployment(n, Environment::Live, DeploymentState::Live);
    r1.published_from = Some(p2.id);
    let wrong = Release {
        kind: ReleaseKind::Publish,
        expected_live: Some(p3.id),
        restored_from: None,
        rollback_of: None,
        approval_id: None,
    };
    let e = s.release(&c, r1.clone(), wrong).await.unwrap_err();
    assert_eq!(e.http_status(), 409);
    assert!(s.get_deployment(a, n, r1.id).await.unwrap().is_none());
    let publish = Release {
        kind: ReleaseKind::Publish,
        expected_live: None,
        restored_from: None,
        rollback_of: None,
        approval_id: None,
    };
    let v1 = s.release(&c, r1.clone(), publish.clone()).await.unwrap();
    assert_eq!(v1.deployment.version_id, Some(v1.version.id));
    assert_eq!(v1.deployment.state, DeploymentState::Live);
    assert_eq!(v1.version.node_id, n);
    assert_eq!(v1.version.actor, Actor::user(ua));
    let row = s.get_app(a, n).await.unwrap().unwrap();
    assert_eq!(row.live_deployment_id, Some(r1.id));
    assert!(row.slug_fixed_at.is_some());
    // A rollback copy: the live deployment is compared, the old one is superseded.
    let r2 = deployment(n, Environment::Live, DeploymentState::Live);
    let stale = s
        .release(&c, r2.clone(), publish.clone())
        .await
        .unwrap_err();
    assert_eq!(stale.http_status(), 409);
    let v2 = s
        .release(
            &c,
            r2.clone(),
            Release {
                kind: ReleaseKind::Rollback,
                expected_live: Some(r1.id),
                restored_from: Some(v1.version.id),
                rollback_of: Some(r1.id),
                approval_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(v2.version.restored_from, Some(v1.version.id));
    assert_eq!(state(r1.id).await, DeploymentState::Superseded);
    let versions = s
        .list_versions(
            a,
            n,
            crate::versions::VersionFilter::default(),
            PageRequest::default(),
        )
        .await
        .unwrap();
    let ids: Vec<VersionId> = versions.items.iter().map(|v| v.id).collect();
    assert_eq!(ids, [v2.version.id, v1.version.id]);
    let (v, d) = s.release_of(a, n, v1.version.id).await.unwrap().unwrap();
    assert_eq!((v.id, d), (v1.version.id, r1.id));
    assert!(
        s.release_of(a, n, VersionId::new())
            .await
            .unwrap()
            .is_none()
    );
    // An approval request of an agent's publish.
    let ap = s
        .insert_approval(
            &c,
            NewApproval {
                id: Uuid::now_v7(),
                app_node_id: n,
                deployment_id: p3.id,
                changes: serde_json::json!({ "roles_added": [] }),
                bound_archive_hash: [7; 32],
                bound_manifest_hash: [8; 32],
                bound_bindings: serde_json::json!([]),
                expires_at: crate::now() + time::Duration::hours(1),
            },
        )
        .await
        .unwrap();
    assert_eq!((ap.status.as_str(), ap.deployment_id), ("pending", p3.id));
    assert_eq!(ap.requested_by, Actor::user(ua));
    // The sweeper ends a ready preview whose expiry passed, and only that one.
    let mut old = deployment(n, Environment::Preview, DeploymentState::Ready);
    old.expires_at = Some(crate::now() - time::Duration::minutes(1));
    let old = s.insert_deployment(&c, old).await.unwrap();
    let mut fresh = deployment(n, Environment::Preview, DeploymentState::Ready);
    fresh.expires_at = Some(crate::now() + time::Duration::days(1));
    let fresh = s.insert_deployment(&c, fresh).await.unwrap();
    let mut ended = Vec::new();
    for _ in 0..100 {
        let got = s.expire_previews(crate::now(), 1000).await.unwrap();
        if got.is_empty() {
            break;
        }
        ended.extend(got);
    }
    assert!(
        ended
            .iter()
            .any(|e| e.id == old.id && e.tenant_id == a && e.app_node_id == n)
    );
    assert!(!ended.iter().any(|e| e.id == fresh.id));
    assert_eq!(state(old.id).await, DeploymentState::Expired);
    assert_eq!(state(fresh.id).await, DeploymentState::Ready);
    // A purge removes the app with its deployments, also while an agent's approval is
    // pending (the approval names a deployment; review W-2s, Kimi 2).
    s.remove_app(a, n).await.unwrap();
    assert!(s.get_app(a, n).await.unwrap().is_none());
    assert!(s.get_deployment(a, n, p3.id).await.unwrap().is_none());
}

/// The deployment writes and reads of card W-2s stay in their tenant.
pub(crate) async fn deployments_stay_in_their_tenant(h: &dyn Harness) {
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let (b, ub) = h.tenant().await;
    let n = h.app_node(a, ua).await;
    let (ca, cb) = (ctx(a, ua), ctx(b, ub));
    s.insert_app(&ca, app(n, &unique("planner")))
        .await
        .unwrap()
        .unwrap();
    let p = s
        .insert_deployment(
            &ca,
            deployment(n, Environment::Preview, DeploymentState::Running),
        )
        .await
        .unwrap();
    let stages = serde_json::json!([]);
    assert!(!s.update_stages(b, p.id, &stages).await.unwrap());
    assert!(s.finish_preview(&cb, p.id, &stages, None).await.is_err());
    assert!(
        s.fail_deployment(&cb, p.id, &stages, None, "image", "x")
            .await
            .is_err()
    );
    assert!(s.get_deployment(b, n, p.id).await.unwrap().is_none());
    // A release under another tenant's app is refused; nothing changes in A.
    let r = deployment(n, Environment::Live, DeploymentState::Live);
    let publish = Release {
        kind: ReleaseKind::Publish,
        expected_live: None,
        restored_from: None,
        rollback_of: None,
        approval_id: None,
    };
    assert!(s.release(&cb, r.clone(), publish.clone()).await.is_err());
    assert!(
        s.get_app(a, n)
            .await
            .unwrap()
            .unwrap()
            .live_deployment_id
            .is_none()
    );
    let v = s.release(&ca, r, publish).await.unwrap();
    assert!(s.release_of(b, n, v.version.id).await.unwrap().is_none());
    assert!(
        s.list_versions(
            b,
            n,
            crate::versions::VersionFilter::default(),
            PageRequest::default()
        )
        .await
        .unwrap()
        .items
        .is_empty()
    );
    assert_eq!(
        s.get_deployment(a, n, p.id).await.unwrap().unwrap().state,
        DeploymentState::Running
    );
}

/// Runs every case.
pub(crate) async fn run_all(h: &dyn Harness) {
    tenant_slugs_span_tenants(h).await;
    apps_stay_in_their_tenant(h).await;
    used_slugs_are_never_given_again(h).await;
    deployments_list_and_releases(h).await;
    previews_releases_and_versions(h).await;
    deployments_stay_in_their_tenant(h).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::store::memory::MemoryAppStore;
    use crate::apps::store::postgres::PgAppStore;
    use crate::journal::memory::MemoryJournal;
    use std::sync::Arc;

    struct Mem(MemoryAppStore);

    #[async_trait]
    impl Harness for Mem {
        fn store(&self) -> &dyn AppStore {
            &self.0
        }
        async fn tenant(&self) -> (TenantId, UserId) {
            (TenantId::new(), UserId::new())
        }
        async fn app_node(&self, _: TenantId, _: UserId) -> NodeId {
            NodeId::new()
        }
    }

    #[tokio::test]
    async fn memory_store_conforms() {
        run_all(&Mem(MemoryAppStore::new(Some(Arc::new(
            MemoryJournal::default(),
        )))))
        .await;
    }

    pub(crate) struct Pg {
        pub pool: sqlx::PgPool,
        pub store: PgAppStore,
    }

    /// A tenant with a root folder and a user, written as the system.
    pub(crate) async fn pg_tenant(pool: &sqlx::PgPool) -> (TenantId, UserId) {
        let (t, u, root) = (TenantId::new(), UserId::new(), NodeId::new());
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            "set local ruum.system = 'on'",
            "set constraints all deferred",
        ] {
            sqlx::query(sql).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into tenants (id, name, kind, plan, root_node_id, created_kind, created_by) \
             values ($1, 't', 'team', 'team', $2, 'user', $3)",
        )
        .bind(t)
        .bind(root)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into users (id, tenant_id, email, email_verified, display_name) \
             values ($1, $2, $3, true, 'x')",
        )
        .bind(u.0)
        .bind(t)
        .bind(format!("{}@example.test", u.0))
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, owner_id, created_kind, \
             created_by, updated_kind, updated_by) \
             values ($1, $2, null, 'root', 'folder', $3, 'user', $3, 'user', $3)",
        )
        .bind(root)
        .bind(t)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (t, u)
    }

    /// A typed `webapp` node under the tenant root, with its manifest version and its
    /// `typed_nodes` row, written as the system.
    pub(crate) async fn pg_app_node(pool: &sqlx::PgPool, t: TenantId, u: UserId) -> NodeId {
        let (node, v) = (NodeId::new(), VersionId::new());
        let (bytes, hash) = crate::nodes::typed_manifest_bytes(&serde_json::json!({}));
        let mut tx = pool.begin().await.unwrap();
        for sql in [
            "set local ruum.system = 'on'",
            "set constraints all deferred",
        ] {
            sqlx::query(sql).execute(&mut *tx).await.unwrap();
        }
        sqlx::query(
            "insert into nodes (id, tenant_id, parent_id, name, kind, type, head_version, \
             owner_id, created_kind, created_by, updated_kind, updated_by) \
             select $1, $2, root_node_id, $3, 'typed', 'webapp', $4, $5, 'user', $5, 'user', $5 \
             from tenants where id = $2",
        )
        .bind(node)
        .bind(t)
        .bind(node.to_string())
        .bind(v)
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query(
            "insert into versions (id, tenant_id, node_id, manifest, encoding, size_bytes, \
             content_hash, actor_kind, actor) values ($1, $2, $3, $4, 'typed_manifest_v1', $5, \
             $6, 'user', $7)",
        )
        .bind(v)
        .bind(t)
        .bind(node)
        .bind(&bytes)
        .bind(bytes.len() as i64)
        .bind(hash.to_vec())
        .bind(u.0)
        .execute(&mut *tx)
        .await
        .unwrap();
        crate::providers::postgres::insert_typed_node(
            &mut tx,
            &crate::providers::store::NewTypedNode {
                tenant_id: t,
                node_id: node,
                provider: crate::apps::PROVIDER_ID.into(),
                type_name: crate::apps::NODE_TYPE.into(),
                desired_version: v,
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        node
    }

    #[async_trait]
    impl Harness for Pg {
        fn store(&self) -> &dyn AppStore {
            &self.store
        }
        async fn tenant(&self) -> (TenantId, UserId) {
            pg_tenant(&self.pool).await
        }
        async fn app_node(&self, t: TenantId, u: UserId) -> NodeId {
            pg_app_node(&self.pool, t, u).await
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_conforms(pool: sqlx::PgPool) {
        let h = Pg {
            store: PgAppStore::new(pool.clone()),
            pool,
        };
        run_all(&h).await;
    }

    /// The slug of an app whose node was purged is retired at once, while the app row still
    /// waits for the provider's `purged` delivery (the node is a hidden tombstone; review
    /// W-2s, 1).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_slug_of_a_purged_app_is_retired_at_once(pool: sqlx::PgPool) {
        let (t, u) = pg_tenant(&pool).await;
        let n = pg_app_node(&pool, t, u).await;
        let store = PgAppStore::new(pool.clone());
        let slug = unique("purged");
        store
            .insert_app(&ctx(t, u), app(n, &slug))
            .await
            .unwrap()
            .unwrap();
        let other = NodeId::new();
        assert_eq!(
            store.slug_state(t, &slug, other).await.unwrap(),
            SlugState::InUse(n)
        );
        // A trashed app (not purged) still holds its slug.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "update nodes set trash_root = id, trashed_at = now() where tenant_id = $1 and id = $2",
        )
        .bind(t)
        .bind(n)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            store.slug_state(t, &slug, other).await.unwrap(),
            SlugState::InUse(n)
        );
        // The purge makes it a tombstone: under the root, with the root as its trash root.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query(
            "update nodes set parent_id = r.id, trash_root = r.id from nodes r \
             where nodes.tenant_id = $1 and nodes.id = $2 and r.tenant_id = $1 \
             and r.parent_id is null",
        )
        .bind(t)
        .bind(n)
        .execute(&mut *tx)
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            store.slug_state(t, &slug, other).await.unwrap(),
            SlugState::Retired
        );
        // The same node gets its slug back (a restore is not possible, but the rule holds).
        assert_eq!(
            store.slug_state(t, &slug, n).await.unwrap(),
            SlugState::Free
        );
    }

    /// The chunk garbage collection reads the chunk lists of app releases (`app_release_v1`)
    /// and of previews that have not ended, and frees the chunks of an expired preview (card
    /// W-2s, P-60; the mutation `apps-deploy-gc-ignores-releases` drops the release lists).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_gc_keeps_release_chunks_and_frees_ended_previews(pool: sqlx::PgPool) {
        use crate::blobs::{BlobStore, ChunkRef, ContentSigner, Hash, Manifest, ManifestFormat};
        let (t, u) = pg_tenant(&pool).await;
        let n = pg_app_node(&pool, t, u).await;
        let store = PgAppStore::new(pool.clone());
        let c = ctx(t, u);
        store
            .insert_app(&c, app(n, &unique("gc")))
            .await
            .unwrap()
            .unwrap();
        let blobs = Arc::new(crate::blobs::memory::MemoryBlobStore::default());
        let mut hashes = Vec::new();
        for name in ["release", "ready", "expired"] {
            let data = format!("{name} archive {}", Uuid::now_v7());
            let h = Hash::of(data.as_bytes());
            blobs
                .put_chunk(t, h, bytes::Bytes::from(data.clone()))
                .await
                .unwrap();
            hashes.push((h, data.len() as u32));
        }
        // The chunk rows come from the store, with each deployment (review W-2s, 3): no row
        // is written by hand here.
        let rows = |(h, size): (Hash, u32)| vec![ChunkRef { hash: h, size }];
        let list = |(h, size): (Hash, u32)| {
            Manifest {
                format: ManifestFormat::ChunksV1,
                chunking: "fastcdc-2020-16k-32k-64k".into(),
                chunks: vec![ChunkRef { hash: h, size }],
                size: u64::from(size),
                content_hash: h,
            }
            .encode()
        };
        // A release: its version names the archive. The deployment row's own list is emptied,
        // so only the version keeps the chunk (as after the row is gone).
        let mut r = deployment(n, Environment::Live, DeploymentState::Live);
        r.archive.chunks = list(hashes[0]);
        r.new_chunks = rows(hashes[0]);
        let released = store
            .release(
                &c,
                r,
                Release {
                    kind: ReleaseKind::Publish,
                    expected_live: None,
                    restored_from: None,
                    rollback_of: None,
                    approval_id: None,
                },
            )
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("update app_deployments set archive_chunks = ''::bytea where id = $1")
            .bind(released.deployment.id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // A ready preview and an expired one.
        let mut ready = deployment(n, Environment::Preview, DeploymentState::Ready);
        ready.archive.chunks = list(hashes[1]);
        ready.new_chunks = rows(hashes[1]);
        store.insert_deployment(&c, ready).await.unwrap();
        let mut gone = deployment(n, Environment::Preview, DeploymentState::Ready);
        gone.archive.chunks = list(hashes[2]);
        gone.new_chunks = rows(hashes[2]);
        let gone = store.insert_deployment(&c, gone).await.unwrap();
        assert!(
            store
                .set_deployment_state(t, gone.id, DeploymentState::Expired)
                .await
                .unwrap()
        );
        let authz: Arc<dyn crate::authz::Authz> =
            Arc::new(crate::authz::memory::MemoryAuthz::default());
        let nodes = Arc::new(crate::nodes::memory::MemoryNodeStore::new(
            authz.clone(),
            Arc::new(MemoryJournal::default()),
        ));
        let content = crate::blobs::postgres::PgContentStore::new(
            pool.clone(),
            authz,
            nodes,
            blobs.clone(),
            ContentSigner::random(),
        );
        let report = content
            .collect_garbage(t, time::Duration::ZERO)
            .await
            .unwrap();
        let recorded: i64 = {
            let mut tx = pool.begin().await.unwrap();
            sqlx::query("set local ruum.system = 'on'")
                .execute(&mut *tx)
                .await
                .unwrap();
            let n = sqlx::query_scalar("select count(*) from chunks where tenant_id = $1")
                .bind(t)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            n
        };
        assert_eq!(
            recorded, 2,
            "the expired preview's row is gone, the others stay"
        );
        assert_eq!(report.deleted, 1, "{report:?}");
        let have = blobs
            .has_chunks(t, &[hashes[0].0, hashes[1].0, hashes[2].0])
            .await
            .unwrap();
        assert_eq!(have, [true, true, false]);
    }

    /// The Postgres store keeps every row in its tenant with row-level security off, so only
    /// the tenant filters of its statements hold (the mutation `apps-tenant-filter-postgres`
    /// drops one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        for table in [
            "tenant_app_settings",
            "apps",
            "retired_app_slugs",
            "app_deployments",
            "app_approvals",
            "journal",
            "typed_nodes",
            "versions",
            "nodes",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "alter table {table} no force row level security"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        let h = Pg {
            store: PgAppStore::new(pool.clone()),
            pool,
        };
        apps_stay_in_their_tenant(&h).await;
        used_slugs_are_never_given_again(&h).await;
        deployments_stay_in_their_tenant(&h).await;
    }
}
