//! The conformance suite of [`RoleStore`]: the memory store and the Postgres store pass the same
//! cases (`tests` below runs both; the Postgres run needs `DATABASE_URL`). Each case checks the
//! rows and the tuples the store writes (an in-memory engine).

use super::*;
use crate::authz::memory::MemoryAuthz;

/// What a case needs from a backend.
#[async_trait]
pub(crate) trait Harness: Send + Sync {
    fn store(&self) -> &dyn RoleStore;
    /// A new tenant and a user of it.
    async fn tenant(&self) -> (TenantId, UserId);
    /// A new web app (its node and `apps` row) in the tenant.
    async fn app(&self, tenant: TenantId, user: UserId) -> NodeId;
}

fn ctx(tenant: TenantId, user: UserId) -> Context {
    Context {
        tenant,
        actor: Actor::user(user),
        scope: None,
        idempotency_key: None,
    }
}

fn new_grant(app: NodeId, role: &str, grantee: Grantee) -> NewRoleGrant {
    NewRoleGrant {
        id: Uuid::now_v7(),
        app_id: app,
        role: role.into(),
        grantee,
    }
}

async fn holders(authz: &MemoryAuthz, app: NodeId, role: &str) -> Vec<String> {
    authz
        .read(&role_object(app, role))
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.relation == "holder")
        .map(|r| r.subject.to_string())
        .collect()
}

/// A grant writes its row, its journal entry and its tuple; a second grant of the same role to
/// the same grantee is a conflict; remove deletes the row and the tuple.
pub(crate) async fn grants_write_rows_and_tuples(h: &dyn Harness) {
    let authz = MemoryAuthz::default();
    let s = h.store();
    let (t, u) = h.tenant().await;
    let app = h.app(t, u).await;
    let c = ctx(t, u);
    let other = UserId::new();
    let w = s
        .add(
            &c,
            &authz,
            new_grant(app, "approver", Grantee::User { id: other }),
        )
        .await
        .unwrap();
    assert!(w.change.seq.is_some(), "a grant writes a journal entry");
    assert_eq!(w.value.granted_by, c.actor);
    assert_eq!(w.value.role, "approver");
    assert_eq!(
        holders(&authz, app, "approver").await,
        [format!("user:{other}")]
    );
    let e = s
        .add(
            &c,
            &authz,
            new_grant(app, "approver", Grantee::User { id: other }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.http_status(), 409);
    s.add(
        &c,
        &authz,
        new_grant(app, "approver", Grantee::TenantMembers),
    )
    .await
    .unwrap();
    assert_eq!(holders(&authz, app, "approver").await.len(), 2);
    let email = s
        .add(
            &c,
            &authz,
            new_grant(
                app,
                "admin",
                Grantee::Email {
                    email: "Fern@Example.test".into(),
                },
            ),
        )
        .await
        .unwrap();
    assert!(
        holders(&authz, app, "admin").await.is_empty(),
        "an email grant has no tuple"
    );
    let dup = s
        .add(
            &c,
            &authz,
            new_grant(
                app,
                "admin",
                Grantee::Email {
                    email: "fern@example.test".into(),
                },
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(dup.http_status(), 409, "emails are compared without case");
    let counts = s.counts(t, app).await.unwrap();
    assert_eq!(counts.get("approver"), Some(&2));
    assert_eq!(counts.get("admin"), Some(&1));
    assert_eq!(
        s.get(t, app, w.value.id).await.unwrap().map(|g| g.grantee),
        Some(Grantee::User { id: other })
    );
    let gone = s
        .remove(&c, &authz, app, w.value.id)
        .await
        .unwrap()
        .unwrap();
    assert!(gone.change.seq.is_some());
    assert_eq!(
        holders(&authz, app, "approver").await,
        [format!("tenant:{t}#member")]
    );
    assert!(s.get(t, app, w.value.id).await.unwrap().is_none());
    assert!(
        s.remove(&c, &authz, app, w.value.id)
            .await
            .unwrap()
            .is_none()
    );
    s.remove(&c, &authz, app, email.value.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.counts(t, app).await.unwrap().get("admin"), None);
}

/// Lists page by ID and filter by role; grants of another app or tenant never show.
pub(crate) async fn lists_page_and_filter(h: &dyn Harness) {
    let authz = MemoryAuthz::default();
    let s = h.store();
    let (t, u) = h.tenant().await;
    let app = h.app(t, u).await;
    let other_app = h.app(t, u).await;
    let c = ctx(t, u);
    let mut ids = Vec::new();
    for _ in 0..5 {
        let g = s
            .add(
                &c,
                &authz,
                new_grant(app, "member", Grantee::User { id: UserId::new() }),
            )
            .await
            .unwrap();
        ids.push(g.value.id);
    }
    s.add(&c, &authz, new_grant(app, "admin", Grantee::User { id: u }))
        .await
        .unwrap();
    s.add(
        &c,
        &authz,
        new_grant(other_app, "member", Grantee::User { id: u }),
    )
    .await
    .unwrap();
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let p = s
            .list(
                t,
                app,
                Some("member"),
                PageRequest {
                    cursor,
                    limit: Some(2),
                },
            )
            .await
            .unwrap();
        assert!(p.items.len() <= 2);
        seen.extend(p.items.iter().map(|g| g.id));
        match p.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    assert_eq!(seen, ids, "oldest first, every grant once");
    let all = s.list(t, app, None, PageRequest::default()).await.unwrap();
    assert_eq!(all.items.len(), 6);
    assert!(all.items.iter().all(|g| g.app_id == app));
}

/// Tenant B never reads, counts or removes a grant of tenant A, also with A's IDs.
pub(crate) async fn grants_stay_in_their_tenant(h: &dyn Harness) {
    let authz = MemoryAuthz::default();
    let s = h.store();
    let (a, ua) = h.tenant().await;
    let (b, ub) = h.tenant().await;
    let app = h.app(a, ua).await;
    let g = s
        .add(
            &ctx(a, ua),
            &authz,
            new_grant(app, "approver", Grantee::User { id: ua }),
        )
        .await
        .unwrap();
    assert!(s.get(b, app, g.value.id).await.unwrap().is_none());
    assert!(
        s.list(b, app, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(s.counts(b, app).await.unwrap().is_empty());
    assert!(
        s.remove(&ctx(b, ub), &authz, app, g.value.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(s.get(a, app, g.value.id).await.unwrap().is_some());
    assert_eq!(holders(&authz, app, "approver").await.len(), 1);
}

/// An email grant binds to the user who proves the email: it becomes a user grant with its
/// tuple; when the user already holds the role, the email grant goes.
pub(crate) async fn email_grants_bind(h: &dyn Harness) {
    let authz = MemoryAuthz::default();
    let s = h.store();
    let (t, u) = h.tenant().await;
    let app = h.app(t, u).await;
    let c = ctx(t, u);
    let fern = UserId::new();
    let g = s
        .add(
            &c,
            &authz,
            new_grant(
                app,
                "approver",
                Grantee::Email {
                    email: "Fern@Example.test".into(),
                },
            ),
        )
        .await
        .unwrap();
    s.add(
        &c,
        &authz,
        new_grant(app, "admin", Grantee::User { id: fern }),
    )
    .await
    .unwrap();
    s.add(
        &c,
        &authz,
        new_grant(
            app,
            "admin",
            Grantee::Email {
                email: "fern@example.test".into(),
            },
        ),
    )
    .await
    .unwrap();
    let as_fern = ctx(t, fern);
    let n = s
        .bind_email(&as_fern, &authz, "fern@example.TEST", fern)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        s.get(t, app, g.value.id).await.unwrap().map(|g| g.grantee),
        Some(Grantee::User { id: fern }),
        "the grant keeps its ID"
    );
    assert_eq!(
        holders(&authz, app, "approver").await,
        [format!("user:{fern}")]
    );
    let counts = s.counts(t, app).await.unwrap();
    assert_eq!(
        counts.get("admin"),
        Some(&1),
        "the duplicate email grant is gone"
    );
    assert_eq!(
        s.bind_email(&as_fern, &authz, "fern@example.test", fern)
            .await
            .unwrap(),
        0
    );
}

pub(crate) async fn run_all(h: &dyn Harness) {
    grants_write_rows_and_tuples(h).await;
    lists_page_and_filter(h).await;
    grants_stay_in_their_tenant(h).await;
    email_grants_bind(h).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::roles::memory::MemoryRoleStore;
    use crate::apps::roles::postgres::PgRoleStore;
    use crate::authz::{ConsistencyToken, outbox};
    use crate::journal::memory::MemoryJournal;

    struct Mem(MemoryRoleStore);

    #[async_trait]
    impl Harness for Mem {
        fn store(&self) -> &dyn RoleStore {
            &self.0
        }
        async fn tenant(&self) -> (TenantId, UserId) {
            (TenantId::new(), UserId::new())
        }
        async fn app(&self, _: TenantId, _: UserId) -> NodeId {
            NodeId::new()
        }
    }

    #[tokio::test]
    async fn memory_store_conforms() {
        run_all(&Mem(MemoryRoleStore::new(Some(Arc::new(
            MemoryJournal::default(),
        )))))
        .await;
    }

    struct Pg {
        pool: sqlx::PgPool,
        store: PgRoleStore,
    }

    /// A tenant with a root folder and a user, written as the system.
    async fn pg_tenant(pool: &sqlx::PgPool) -> (TenantId, UserId) {
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

    /// A typed `webapp` node under the tenant root (with its manifest version and its
    /// `typed_nodes` row) and its `apps` row, written as the system.
    async fn pg_app(pool: &sqlx::PgPool, t: TenantId, u: UserId) -> NodeId {
        let (node, v) = (NodeId::new(), crate::ids::VersionId::new());
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
        sqlx::query("insert into apps (tenant_id, node_id, slug) values ($1, $2, $3)")
            .bind(t)
            .bind(node)
            .bind(format!("app-{}", &node.0.simple().to_string()[20..]))
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        node
    }

    #[async_trait]
    impl Harness for Pg {
        fn store(&self) -> &dyn RoleStore {
            &self.store
        }
        async fn tenant(&self) -> (TenantId, UserId) {
            pg_tenant(&self.pool).await
        }
        async fn app(&self, t: TenantId, u: UserId) -> NodeId {
            pg_app(&self.pool, t, u).await
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_conforms(pool: sqlx::PgPool) {
        let h = Pg {
            store: PgRoleStore::new(pool.clone()),
            pool,
        };
        run_all(&h).await;
    }

    /// Only the tenant filters of the statements keep tenants apart when row-level security is
    /// off on `app_role_grants` (the mutation `apps-roles-tenant-filter-postgres` drops one).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_store_filters_by_tenant_without_rls(pool: sqlx::PgPool) {
        sqlx::query("alter table app_role_grants no force row level security")
            .execute(&pool)
            .await
            .unwrap();
        let h = Pg {
            store: PgRoleStore::new(pool.clone()),
            pool,
        };
        grants_stay_in_their_tenant(&h).await;
    }

    /// An engine that refuses every write (SpiceDB down, or a crash after the commit).
    struct Down;

    #[async_trait]
    impl Authz for Down {
        async fn check(
            &self,
            _: &SubjectRef,
            _: &str,
            _: &ObjectRef,
            _: Option<&ConsistencyToken>,
        ) -> Result<bool> {
            Ok(false)
        }
        async fn check_bulk(&self, _: &SubjectRef, _: &str, o: &[ObjectRef]) -> Result<Vec<bool>> {
            Ok(vec![false; o.len()])
        }
        async fn write(&self, _: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
            Err(Error::Internal("the engine is down".into()))
        }
        async fn read(&self, _: &ObjectRef) -> Result<Vec<Relationship>> {
            Ok(Vec::new())
        }
    }

    /// A grant and its removal commit their rows while the engine is down; the outbox keeps the
    /// tuple updates, and the worker applies them in order, so the graph matches the rows.
    #[sqlx::test(migrations = "../../migrations")]
    async fn row_and_tuple_meet_after_the_outbox_runs(pool: sqlx::PgPool) {
        let store = PgRoleStore::new(pool.clone());
        let (t, u) = pg_tenant(&pool).await;
        let app = pg_app(&pool, t, u).await;
        let c = ctx(t, u);
        let fern = UserId::new();
        let kept = store
            .add(
                &c,
                &Down,
                new_grant(app, "approver", Grantee::User { id: fern }),
            )
            .await
            .unwrap();
        let gone = store
            .add(
                &c,
                &Down,
                new_grant(app, "admin", Grantee::User { id: fern }),
            )
            .await
            .unwrap();
        store
            .remove(&c, &Down, app, gone.value.id)
            .await
            .unwrap()
            .unwrap();
        assert!(store.get(t, app, kept.value.id).await.unwrap().is_some());
        assert_eq!(outbox::pending_count(&pool).await.unwrap(), 3);
        let authz = MemoryAuthz::default();
        outbox::apply_pending(&pool, &authz, 100).await.unwrap();
        assert_eq!(outbox::pending_count(&pool).await.unwrap(), 0);
        assert_eq!(
            holders(&authz, app, "approver").await,
            [format!("user:{fern}")]
        );
        assert!(holders(&authz, app, "admin").await.is_empty());
    }
}
