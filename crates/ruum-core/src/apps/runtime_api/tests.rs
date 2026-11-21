//! The pure rules of the runtime API's core side: sealing, the topic rule, the subject cache.
//! The service runs end to end in `crates/ruum-runtime-api/tests/` (the T-6 world).

use super::*;

#[test]
fn a_sealed_subject_opens_only_with_its_key() {
    let tk = [3u8; 32];
    let app = NodeId(Uuid::now_v7());
    let user = Uuid::now_v7();
    let k = sealing_key(&tk, app, "user");
    let s = seal(&k, user);
    assert_ne!(s, user);
    assert_eq!(open(&k, s), user);
    // Another app, another tenant key and the agent key each give another value.
    for other in [
        sealing_key(&tk, NodeId(Uuid::now_v7()), "user"),
        sealing_key(&[4u8; 32], app, "user"),
        sealing_key(&tk, app, "agent"),
    ] {
        assert_ne!(seal(&other, user), s);
        assert_ne!(open(&other, s), user);
    }
    // The value has the UUID text form of the schema.
    assert!(Uuid::parse_str(&s.to_string()).is_ok());
}

#[test]
fn a_topic_is_declared_by_an_events_api() {
    let m = serde_json::json!({
        "apis": {
            "orders": { "protocol": "events", "visibility": "internal", "spec": "a.yaml", "scopes": ["read"] },
            "rest": { "protocol": "http", "visibility": "internal", "spec": "b.yaml", "scopes": ["read"] }
        }
    });
    assert!(topic_declared(&m, "orders"));
    assert!(topic_declared(&m, "orders.created"));
    for t in [
        "ordersx",
        "orders_created",
        "rest",
        "rest.created",
        "other.orders",
    ] {
        assert!(!topic_declared(&m, t), "{t}");
    }
    assert!(!topic_declared(&serde_json::json!({}), "orders"));
}

#[test]
fn the_subject_cache_is_bounded_per_tenant() {
    let mut s = Subjects::default();
    let now = Instant::now();
    let app = NodeId(Uuid::now_v7());
    let a = TenantId(Uuid::now_v7());
    let b = TenantId(Uuid::now_v7());
    s.put(b, app, "pairwise:b", Uuid::now_v7(), now);
    for i in 0..SUBJECTS_PER_TENANT + 10 {
        s.put(
            a,
            app,
            &format!("pairwise:{i}"),
            Uuid::now_v7(),
            now + Duration::from_millis(i as u64),
        );
    }
    assert_eq!(s.by_tenant[&a].len(), SUBJECTS_PER_TENANT);
    // Tenant b keeps its entry: a's flood evicts only a's own oldest entries.
    assert!(s.get(b, app, "pairwise:b", now).is_some());
    assert!(s.get(a, app, "pairwise:0", now).is_none());
    assert_eq!(s.total, SUBJECTS_PER_TENANT + 1);
    // An entry ends after its TTL.
    assert!(s.get(b, app, "pairwise:b", now + SUBJECT_TTL).is_none());
}

// ------------------------------------------------------------------------------- Postgres

mod postgres {
    use super::*;
    use crate::actor::Actor;
    use crate::apps::store::conformance::{app, deployment};
    use crate::apps::store::postgres::PgAppStore;
    use crate::apps::store::{AppStore, BindingKind, DeploymentState, Environment, NewBinding};
    use crate::ids::{UserId, VersionId};

    fn ctx(tenant: TenantId, user: UserId) -> crate::actor::Context {
        crate::actor::Context {
            tenant,
            actor: Actor::user(user),
            scope: None,
            idempotency_key: None,
        }
    }

    /// A tenant with a root and a user, written as the system (as the store conformance does).
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

    /// A typed `webapp` node under the tenant root, with its manifest version and its
    /// `typed_nodes` row.
    async fn pg_app_node(pool: &sqlx::PgPool, t: TenantId, u: UserId) -> NodeId {
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

    /// The binding rows the runtime API reads stay in the caller's tenant with row-level
    /// security off, so only the statement's tenant filter holds (the mutation
    /// `runtime-api-tenant-filter-postgres` drops it): a caller of another tenant that names
    /// the app and the principal of tenant A gets none of A's rows.
    #[sqlx::test(migrations = "../../migrations")]
    async fn postgres_bound_rows_stay_in_their_tenant(pool: sqlx::PgPool) {
        for table in [
            "apps",
            "app_deployments",
            "app_bindings",
            "nodes",
            "versions",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "alter table {table} no force row level security"
            )))
            .execute(&pool)
            .await
            .unwrap();
        }
        let store = PgAppStore::new(pool.clone());
        let (a, ua) = pg_tenant(&pool).await;
        let (b, _) = pg_tenant(&pool).await;
        let n = pg_app_node(&pool, a, ua).await;
        let ca = ctx(a, ua);
        store
            .insert_app(&ca, app(n, &format!("r3-{}", Uuid::now_v7().simple())))
            .await
            .unwrap()
            .unwrap();
        let p = store
            .insert_deployment(
                &ca,
                deployment(n, Environment::Preview, DeploymentState::Ready),
            )
            .await
            .unwrap();
        let authz = crate::authz::memory::MemoryAuthz::default();
        store
            .grant_bindings(
                &ca,
                &authz,
                vec![NewBinding {
                    id: Uuid::now_v7(),
                    app_node_id: n,
                    deployment_id: p.id,
                    principal_id: p.id,
                    name: "docs".into(),
                    kind: BindingKind::Node,
                    target_node_id: n,
                    role: Some(crate::authz::Role::View),
                    api: None,
                    scopes: Vec::new(),
                    on_behalf_of_user: true,
                }],
            )
            .await
            .unwrap();
        let mine = Caller {
            tenant_id: a,
            app_id: n,
            principal: p.id,
            environment: AppEnv::Preview,
        };
        assert_eq!(bound_rows(&store, &mine).await.unwrap().len(), 1);
        let forged = Caller {
            tenant_id: b,
            ..mine.clone()
        };
        assert!(bound_rows(&store, &forged).await.unwrap().is_empty());
    }
}
