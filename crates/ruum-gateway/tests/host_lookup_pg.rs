//! The host lookup on Postgres (card G-1a; `docs/STEP3.md` "System lookups"; migration 0013):
//! `ruum_app_host_lookup` returns only `(tenant_id, app_id, deployment_id)` of the live deployment
//! or of a ready preview, runs with the system flag of its own body only, and is all that the
//! gateway's database role may use. `#[sqlx::test]`: one database per test (`DATABASE_URL`).

use ruum_core::actor::{Actor, Context};
use ruum_core::apps::settings::AppSettings;
use ruum_core::apps::store::postgres::PgAppStore;
use ruum_core::apps::store::{
    AppStore, Archive, DeploymentSource, DeploymentState, Environment, NewApp, NewDeployment,
};
use ruum_core::{NodeId, TenantId, UserId, VersionId};
use ruum_gateway::host::{AppHost, HostLookup, PgHostLookup, Resolved};
use sqlx::PgPool;
use uuid::Uuid;

/// A tenant with a root folder and a user, written as the system.
async fn tenant(pool: &PgPool) -> (TenantId, UserId) {
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

/// A typed `webapp` node under the tenant root with its manifest version and `typed_nodes` row.
async fn app_node(pool: &PgPool, t: TenantId, u: UserId) -> NodeId {
    let (node, v) = (NodeId::new(), VersionId::new());
    let (bytes, hash) = ruum_core::nodes::typed_manifest_bytes(&serde_json::json!({}));
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
    ruum_core::providers::postgres::insert_typed_node(
        &mut tx,
        &ruum_core::providers::store::NewTypedNode {
            tenant_id: t,
            node_id: node,
            provider: ruum_core::apps::PROVIDER_ID.into(),
            type_name: ruum_core::apps::NODE_TYPE.into(),
            desired_version: v,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    node
}

fn deployment(app: NodeId, env: Environment, state: DeploymentState) -> NewDeployment {
    let archive = Uuid::now_v7().to_string();
    NewDeployment {
        id: Uuid::now_v7(),
        app_node_id: app,
        environment: env,
        state,
        short_id: ruum_core::apps::slugs::short_id(),
        label: None,
        draft: None,
        new_chunks: Vec::new(),
        source: DeploymentSource {
            kind: "file".into(),
            node_id: None,
            version_id: None,
            files: None,
        },
        archive: Archive {
            content_hash: *blake3::hash(archive.as_bytes()).as_bytes(),
            size: archive.len() as i64,
            chunks: Vec::new(),
        },
        manifest: Some(serde_json::json!({ "static": { "root": "." } })),
        stages: serde_json::json!([]),
        published_from: None,
        expires_at: None,
    }
}

struct Seed {
    tenant: TenantId,
    garden: NodeId,
    live: Uuid,
    preview: Uuid,
    preview_short: String,
    failed_short: String,
    shed_preview_short: String,
    expired_short: String,
}

/// Tenant `acme` with the app `garden` (a live release, a ready preview, a failed preview, an
/// expired preview) and the app `shed` (a ready preview).
async fn seed(pool: &PgPool) -> Seed {
    let store = PgAppStore::new(pool.clone());
    let (t, u) = tenant(pool).await;
    store
        .ensure_tenant_settings(t, &["acme".to_string()])
        .await
        .unwrap();
    let ctx = Context {
        tenant: t,
        actor: Actor::user(u),
        scope: None,
        idempotency_key: None,
    };
    let mut apps = Vec::new();
    for slug in ["garden", "shed"] {
        let node = app_node(pool, t, u).await;
        store
            .insert_app(
                &ctx,
                NewApp {
                    node_id: node,
                    slug: slug.into(),
                    settings: AppSettings::default(),
                },
            )
            .await
            .unwrap()
            .unwrap();
        apps.push(node);
    }
    let (garden, shed) = (apps[0], apps[1]);
    let live = store
        .insert_deployment(
            &ctx,
            deployment(garden, Environment::Live, DeploymentState::Live),
        )
        .await
        .unwrap();
    let preview = store
        .insert_deployment(
            &ctx,
            deployment(garden, Environment::Preview, DeploymentState::Ready),
        )
        .await
        .unwrap();
    let failed = store
        .insert_deployment(
            &ctx,
            deployment(garden, Environment::Preview, DeploymentState::Failed),
        )
        .await
        .unwrap();
    let mut old = deployment(garden, Environment::Preview, DeploymentState::Ready);
    old.expires_at = Some(ruum_core::now() - time::Duration::minutes(1));
    let expired = store.insert_deployment(&ctx, old).await.unwrap();
    let shed_preview = store
        .insert_deployment(
            &ctx,
            deployment(shed, Environment::Preview, DeploymentState::Ready),
        )
        .await
        .unwrap();
    Seed {
        tenant: t,
        garden,
        live: live.id,
        preview: preview.id,
        preview_short: preview.short_id,
        failed_short: failed.short_id,
        shed_preview_short: shed_preview.short_id,
        expired_short: expired.short_id,
    }
}

fn host(app: &str, tenant: &str, short: Option<&str>) -> AppHost {
    AppHost {
        tenant_slug: tenant.into(),
        app_slug: app.into(),
        short_id: short.map(str::to_string),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_lookup_finds_only_serving_deployments(pool: PgPool) {
    let s = seed(&pool).await;
    // The test's own connection: no tenant context and no system flag. The tables force
    // row-level security, so only the function's own system flag finds the rows.
    let lookup = PgHostLookup::new(pool.clone());
    let get = |h: AppHost| {
        let lookup = &lookup;
        async move { lookup.lookup(&h).await.unwrap() }
    };
    assert_eq!(
        get(host("garden", "acme", None)).await,
        Some(Resolved {
            tenant_id: s.tenant,
            app_id: s.garden,
            deployment_id: s.live,
        })
    );
    assert_eq!(
        get(host("garden", "acme", Some(&s.preview_short))).await,
        Some(Resolved {
            tenant_id: s.tenant,
            app_id: s.garden,
            deployment_id: s.preview,
        })
    );
    for (what, h) in [
        (
            "a failed preview",
            host("garden", "acme", Some(&s.failed_short)),
        ),
        (
            "an expired preview",
            host("garden", "acme", Some(&s.expired_short)),
        ),
        (
            "a preview of another app",
            host("garden", "acme", Some(&s.shed_preview_short)),
        ),
        (
            "an app without a live deployment",
            host("shed", "acme", None),
        ),
        ("an unknown app", host("nosuchapp", "acme", None)),
        ("an unknown tenant", host("garden", "nosuchtenant", None)),
        (
            "an unknown short ID",
            host("garden", "acme", Some("zzzzzzzz")),
        ),
        ("a reserved app slug", host("admin", "acme", None)),
        ("a reserved tenant slug", host("garden", "content", None)),
    ] {
        assert_eq!(get(h).await, None, "{what}");
    }
    // An app in the trash answers nothing.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("set local ruum.system = 'on'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "update nodes set trashed_at = now(), trash_root = id where tenant_id = $1 and id = $2",
    )
    .bind(s.tenant)
    .bind(s.garden)
    .execute(&mut *tx)
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(get(host("garden", "acme", None)).await, None);
    assert_eq!(
        get(host("garden", "acme", Some(&s.preview_short))).await,
        None
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn the_function_returns_only_ids_and_runs_with_nothing_else(pool: PgPool) {
    let s = seed(&pool).await;
    // Exactly three result columns, all IDs.
    let cols: Vec<(String, String)> = sqlx::query_as(
        "select p.parameter_name::text, p.data_type::text \
         from information_schema.parameters p join information_schema.routines r \
         on r.specific_name = p.specific_name \
         where r.routine_name = 'ruum_app_host_lookup' and p.parameter_mode = 'OUT' \
         order by p.ordinal_position",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        cols,
        vec![
            ("tenant_id".to_string(), "uuid".to_string()),
            ("app_id".to_string(), "uuid".to_string()),
            ("deployment_id".to_string(), "uuid".to_string()),
        ]
    );
    // SECURITY DEFINER with a fixed search_path; the body turns the system flag on for itself
    // and puts the caller's value back.
    let (definer, config, body): (bool, Vec<String>, String) = sqlx::query_as(
        "select prosecdef, coalesce(proconfig, '{}'), prosrc from pg_proc \
         where proname = 'ruum_app_host_lookup'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(definer, "the lookup runs as its owner");
    assert!(
        config
            .iter()
            .any(|c| c == "search_path=pg_catalog, pg_temp"),
        "{config:?}"
    );
    assert!(body.contains("set_config('ruum.system', 'on', true)"));
    // Nobody but the gateway's role may execute it.
    let public: bool = sqlx::query_scalar(
        "select has_function_privilege('public', 'ruum_app_host_lookup(text, text, text)', \
         'execute')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!public, "PUBLIC may execute the lookup");
    // The flag does not leak: after a call, the same transaction reads no tenant row.
    let mut tx = pool.begin().await.unwrap();
    let found: Option<(Uuid, Uuid, Uuid)> =
        sqlx::query_as("select * from ruum_app_host_lookup('acme', 'garden', null)")
            .fetch_optional(&mut *tx)
            .await
            .unwrap();
    assert_eq!(found, Some((s.tenant.0, s.garden.0, s.live)));
    let flag: Option<String> = sqlx::query_scalar("select current_setting('ruum.system', true)")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_ne!(flag.as_deref(), Some("on"));
    let rows: i64 = sqlx::query_scalar("select count(*) from apps")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a transaction without a tenant reads no app row");
    tx.rollback().await.unwrap();
    // As the gateway's role: the lookup works, and no tenant table can be read. The role is
    // cluster-wide; the test role must be a member of it (CI grants it: `ruum_gateway` to
    // `ruum_test`). With RUUM_REQUIRE_GATEWAY_ROLE=1 (CI) a role that cannot be used fails the
    // test instead of skipping this part (review-g1a 1).
    let require = std::env::var("RUUM_REQUIRE_GATEWAY_ROLE").is_ok_and(|v| v == "1");
    let mut tx = pool.begin().await.unwrap();
    let set_role = sqlx::query("set local role ruum_gateway")
        .execute(&mut *tx)
        .await;
    match set_role {
        Ok(_) => {
            let found: Option<(Uuid, Uuid, Uuid)> =
                sqlx::query_as("select * from ruum_app_host_lookup('acme', 'garden', null)")
                    .fetch_optional(&mut *tx)
                    .await
                    .unwrap();
            assert_eq!(found, Some((s.tenant.0, s.garden.0, s.live)));
            for table in [
                "apps",
                "app_deployments",
                "tenant_app_settings",
                "app_sign_in_codes",
                "app_sessions",
                "nodes",
                "users",
                "tenants",
            ] {
                sqlx::query("savepoint t").execute(&mut *tx).await.unwrap();
                let read = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "select 1 from {table} limit 1"
                )))
                .execute(&mut *tx)
                .await;
                assert!(read.is_err(), "the gateway's role reads {table}");
                sqlx::query("rollback to savepoint t")
                    .execute(&mut *tx)
                    .await
                    .unwrap();
            }
        }
        Err(e) => {
            assert!(
                !require,
                "RUUM_REQUIRE_GATEWAY_ROLE=1, but the role ruum_gateway is not usable: {e}"
            );
            println!(
                "the role ruum_gateway is not usable on this cluster (no CREATEROLE for the \
                 migration, or no membership); the checks above stand alone"
            );
        }
    }
    tx.rollback().await.unwrap();
}
