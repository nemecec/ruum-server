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

/// The purge of an app deletes all its grants (user, `tenant_members`, email) with their
/// `holder` tuples and the extra tuple updates, and nothing of another app or tenant.
pub(crate) async fn remove_app_deletes_its_grants(h: &dyn Harness) {
    let authz = MemoryAuthz::default();
    let s = h.store();
    let (t, u) = h.tenant().await;
    let (b, ub) = h.tenant().await;
    let app = h.app(t, u).await;
    let kept = h.app(t, u).await;
    let theirs = h.app(b, ub).await;
    let c = ctx(t, u);
    let fern = UserId::new();
    for grantee in [
        Grantee::User { id: fern },
        Grantee::TenantMembers,
        Grantee::Email {
            email: "fern@example.test".into(),
        },
    ] {
        s.add(&c, &authz, new_grant(app, "approver", grantee))
            .await
            .unwrap();
    }
    s.add(
        &c,
        &authz,
        new_grant(kept, "approver", Grantee::User { id: fern }),
    )
    .await
    .unwrap();
    s.add(
        &ctx(b, ub),
        &authz,
        new_grant(theirs, "approver", Grantee::User { id: ub }),
    )
    .await
    .unwrap();
    authz
        .write(vec![RelationshipUpdate::Touch(active_tuple(
            app, "approver",
        ))])
        .await
        .unwrap();
    // Another tenant removes nothing, also with this app's ID.
    assert_eq!(s.remove_app(b, &authz, app, Vec::new()).await.unwrap(), 0);
    assert_eq!(holders(&authz, app, "approver").await.len(), 2);
    let n = s
        .remove_app(
            t,
            &authz,
            app,
            vec![RelationshipUpdate::Delete(active_tuple(app, "approver"))],
        )
        .await
        .unwrap();
    assert_eq!(n, 3);
    assert!(s.counts(t, app).await.unwrap().is_empty());
    assert!(
        authz
            .read(&role_object(app, "approver"))
            .await
            .unwrap()
            .is_empty(),
        "no holder and no flag is left"
    );
    assert_eq!(s.counts(t, kept).await.unwrap().get("approver"), Some(&1));
    assert_eq!(holders(&authz, kept, "approver").await.len(), 1);
    assert_eq!(s.counts(b, theirs).await.unwrap().get("approver"), Some(&1));
}

/// The app role part of `removeMember` (backlog S-43): every user grant of the user in the
/// tenant goes, on every app, with its `holder` tuple and an `app.role_revoked` entry; the
/// grants of other users, `tenant_members` and email grants stay, and so does the same user's
/// grant in another tenant.
pub(crate) async fn remove_user_deletes_only_that_users_grants(h: &dyn Harness) {
    let authz = MemoryAuthz::default();
    let s = h.store();
    let (t, u) = h.tenant().await;
    let (b, ub) = h.tenant().await;
    let app = h.app(t, u).await;
    let second = h.app(t, u).await;
    let theirs = h.app(b, ub).await;
    let c = ctx(t, u);
    let fern = UserId::new();
    let mut fern_grants = Vec::new();
    for (on, role) in [(app, "approver"), (app, "admin"), (second, "approver")] {
        let g = s
            .add(&c, &authz, new_grant(on, role, Grantee::User { id: fern }))
            .await
            .unwrap();
        fern_grants.push(g.value.id);
    }
    let mut kept = Vec::new();
    for grantee in [
        Grantee::User { id: u },
        Grantee::TenantMembers,
        Grantee::Email {
            email: "fern@example.test".into(),
        },
    ] {
        kept.push(
            s.add(&c, &authz, new_grant(app, "approver", grantee))
                .await
                .unwrap()
                .value
                .id,
        );
    }
    let elsewhere = s
        .add(
            &ctx(b, ub),
            &authz,
            new_grant(theirs, "approver", Grantee::User { id: fern }),
        )
        .await
        .unwrap()
        .value
        .id;
    // The removal of the same user from a third tenant leaves the grants of the others.
    let (third, u3) = h.tenant().await;
    assert_eq!(
        s.remove_user(&ctx(third, u3), &authz, fern).await.unwrap(),
        0
    );
    assert_eq!(s.counts(t, app).await.unwrap().get("admin"), Some(&1));
    assert_eq!(holders(&authz, theirs, "approver").await.len(), 1);
    let n = s.remove_user(&c, &authz, fern).await.unwrap();
    assert_eq!(n, 3);
    for id in &fern_grants {
        assert!(s.get(t, app, *id).await.unwrap().is_none());
        assert!(s.get(t, second, *id).await.unwrap().is_none());
    }
    for id in &kept {
        assert!(s.get(t, app, *id).await.unwrap().is_some(), "{id} stays");
    }
    let fern_subject = format!("user:{fern}");
    for (on, role) in [(app, "approver"), (app, "admin"), (second, "approver")] {
        assert!(
            !holders(&authz, on, role).await.contains(&fern_subject),
            "the holder tuple of {role} is gone"
        );
    }
    assert_eq!(
        holders(&authz, app, "approver").await.len(),
        2,
        "the user grant of {u} and tenant_members stay"
    );
    assert!(s.get(b, theirs, elsewhere).await.unwrap().is_some());
    assert_eq!(holders(&authz, theirs, "approver").await, [fern_subject]);
    assert_eq!(s.remove_user(&c, &authz, fern).await.unwrap(), 0);
}

pub(crate) async fn run_all(h: &dyn Harness) {
    grants_write_rows_and_tuples(h).await;
    lists_page_and_filter(h).await;
    grants_stay_in_their_tenant(h).await;
    email_grants_bind(h).await;
    remove_app_deletes_its_grants(h).await;
    remove_user_deletes_only_that_users_grants(h).await;
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
        remove_app_deletes_its_grants(&h).await;
        remove_user_deletes_only_that_users_grants(&h).await;
    }

    /// After the grants of an app are removed, the app store's purge deletes the app row
    /// (`app_role_grants` refers to `apps`, so it failed while grants were left).
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_app_purge_passes_after_the_grants_go(pool: sqlx::PgPool) {
        use crate::apps::AppStore as _;
        let store = PgRoleStore::new(pool.clone());
        let apps = crate::apps::store::postgres::PgAppStore::new(pool.clone());
        let (t, u) = pg_tenant(&pool).await;
        let app = pg_app(&pool, t, u).await;
        let authz = MemoryAuthz::default();
        store
            .add(
                &ctx(t, u),
                &authz,
                new_grant(app, "approver", Grantee::TenantMembers),
            )
            .await
            .unwrap();
        assert!(
            apps.remove_app(t, app).await.is_err(),
            "the grant row holds the app row"
        );
        assert_eq!(
            store.remove_app(t, &authz, app, Vec::new()).await.unwrap(),
            1
        );
        apps.remove_app(t, app).await.unwrap();
        assert!(apps.get_app(t, app).await.unwrap().is_none());
        assert!(holders(&authz, app, "approver").await.is_empty());
    }

    /// `removeMember` on the Postgres tenant store (backlog S-43): a removed member and a guest
    /// who leaves lose their app role grants, with an `app.role_revoked` entry each, and the
    /// deletes of their `holder` tuples are in the outbox of the same transaction. The grants
    /// of others and the removed member's grant in another tenant stay. (The mutation
    /// `apps-roles-member-removal-keeps-grants-postgres` skips the delete.)
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_member_removal_deletes_role_grants_and_tuples(pool: sqlx::PgPool) {
        use crate::identity::tests::{postgres, user};
        use crate::tenants::MemberRole;
        let a = postgres(pool.clone());
        let owner = user(&a, "owner").await;
        let member = user(&a, "member").await;
        let guest = user(&a, "guest").await;
        let team = a
            .tenants
            .create_team(&Actor::user(owner.id), "Acme".into())
            .await
            .unwrap();
        let other = a
            .tenants
            .create_team(&Actor::user(owner.id), "Other".into())
            .await
            .unwrap();
        for (t, u, role) in [
            (team.id, &member, MemberRole::Member),
            (team.id, &guest, MemberRole::Guest),
            (other.id, &member, MemberRole::Member),
        ] {
            let (_, token) = a
                .tenants
                .invite(&ctx(t, owner.id), u.email.clone(), role, None)
                .await
                .unwrap();
            a.tenants
                .accept_invitation(&Actor::user(u.id), &token)
                .await
                .unwrap();
        }
        let app = pg_app(&pool, team.id, owner.id).await;
        let theirs = pg_app(&pool, other.id, owner.id).await;
        let store = PgRoleStore::new(pool.clone());
        let authz = MemoryAuthz::default();
        let oc = ctx(team.id, owner.id);
        for (role, to) in [
            ("approver", member.id),
            ("admin", member.id),
            ("approver", guest.id),
            ("approver", owner.id),
        ] {
            store
                .add(&oc, &authz, new_grant(app, role, Grantee::User { id: to }))
                .await
                .unwrap();
        }
        let elsewhere = store
            .add(
                &ctx(other.id, owner.id),
                &authz,
                new_grant(theirs, "approver", Grantee::User { id: member.id }),
            )
            .await
            .unwrap()
            .value
            .id;
        // Both roles are active (the flags of a live manifest), so a holder may use the app.
        let mut flags = Vec::new();
        for (on, role) in [(app, "approver"), (app, "admin"), (theirs, "approver")] {
            flags.push(RelationshipUpdate::Touch(active_tuple(on, role)));
            flags.push(RelationshipUpdate::Touch(active_role_tuple(on, role)));
        }
        authz.write(flags).await.unwrap();
        let uses = |u: UserId, on: NodeId| {
            let authz = &authz;
            async move {
                authz
                    .check(&SubjectRef::user(u), "use", &ObjectRef::node(on), None)
                    .await
                    .unwrap()
            }
        };
        assert!(uses(member.id, app).await && uses(guest.id, app).await);
        a.tenants.remove_member(&oc, member.id).await.unwrap();
        a.tenants
            .remove_member(&ctx(team.id, guest.id), guest.id)
            .await
            .unwrap();
        // The area has no engine: the tuple deletes wait in the outbox, as after a crash.
        outbox::apply_pending(&pool, &authz, 1000).await.unwrap();
        let left: Vec<Grantee> = store
            .list(team.id, app, None, PageRequest::default())
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|g| g.grantee)
            .collect();
        assert_eq!(left, [Grantee::User { id: owner.id }]);
        assert_eq!(
            holders(&authz, app, "approver").await,
            [format!("user:{}", owner.id)]
        );
        assert!(holders(&authz, app, "admin").await.is_empty());
        assert!(!uses(member.id, app).await, "a removed member uses no app");
        assert!(!uses(guest.id, app).await, "a guest who left uses no app");
        assert!(uses(owner.id, app).await);
        // The other tenant keeps the member and the grant.
        assert!(
            store
                .get(other.id, theirs, elsewhere)
                .await
                .unwrap()
                .is_some()
        );
        assert!(uses(member.id, theirs).await);
        assert_eq!(
            holders(&authz, theirs, "approver").await,
            [format!("user:{}", member.id)]
        );
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("set local ruum.system = 'on'")
            .execute(&mut *tx)
            .await
            .unwrap();
        let revoked: i64 = sqlx::query_scalar(
            "select count(*) from journal where tenant_id = $1 and type = 'app.role_revoked'",
        )
        .bind(team.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(revoked, 3, "one entry per removed grant");
        let data: serde_json::Value = sqlx::query_scalar(
            "select data from audit where tenant_id = $1 and action = 'member.removed' \
             and target_id = $2",
        )
        .bind(team.id)
        .bind(member.id.to_string())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(data["app_role_grants_removed"], 2);
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
