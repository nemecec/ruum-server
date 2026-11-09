//! Making a database and its roles, the Ruum schemas, the meta data and the lens rebuild.
//!
//! Every statement here is platform DDL of `ruum_dbhost` (decision 0004-1). Objects owned by
//! `mask` are made under `SET LOCAL ROLE <mask>` (`ruum_dbhost` holds `SET`-only membership in
//! it, decision 0004-34 a). The base-table rights of `mask` and `secret_read` come from
//! `ALTER DEFAULT PRIVILEGES` that the platform runs once in an `owner` session (0004-34 b).

use super::errors::internal;
use super::lens::{self, LensRoles, ident};
use super::pool::search_path;
use super::{DbState, Meta, PostgresEngine, catalog, neutral_name};
use crate::error::EngineError;
use crate::key::PseudonymKey;
use crate::model::*;
use crate::role::RoleKind;
use sqlx::{Connection, PgConnection, Row};
use std::sync::Arc;
use uuid::Uuid;

/// Runs a script of platform statements (simple protocol) on `conn`.
pub(crate) async fn run(
    conn: &mut PgConnection,
    what: &str,
    sql: String,
) -> Result<(), EngineError> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
        .execute(&mut *conn)
        .await
        .map(|_| ())
        .map_err(|e| internal(what, e))
}

/// New role names, one per kind of [`RoleKind::ALL`].
pub(crate) fn new_roles() -> Vec<EngineRole> {
    RoleKind::ALL
        .into_iter()
        .map(|kind| EngineRole {
            kind,
            binding: None,
            role_name: neutral_name("r_"),
        })
        .collect()
}

fn role_of(roles: &[EngineRole], kind: RoleKind) -> &str {
    roles
        .iter()
        .find(|r| r.kind == kind)
        .map(|r| r.role_name.as_str())
        .expect("a role of every kind")
}

/// Makes the roles of a database in the cluster: the LOGIN kinds without a password (the pool
/// sets one), `mask` NOLOGIN, and `ruum_dbhost`'s SET-only membership in `mask`.
pub(crate) async fn create_roles(
    engine: &PostgresEngine,
    roles: &[EngineRole],
) -> Result<(), EngineError> {
    let mut sql = String::new();
    for role in roles {
        let name = ident(&role.role_name);
        if role.kind == RoleKind::Mask {
            sql.push_str(&format!(
                "create role {name} nologin noinherit nosuperuser nocreatedb nocreaterole \
                   noreplication nobypassrls;\n\
                 grant {name} to ruum_dbhost with inherit false, set true;\n"
            ));
        } else {
            sql.push_str(&format!(
                "create role {name} login noinherit nosuperuser nocreatedb nocreaterole \
                   noreplication nobypassrls password null;\n\
                 alter role {name} set search_path = {};\n",
                search_path(role.kind)
            ));
        }
    }
    let mut admin = engine.admin().await?;
    run(&mut admin, "create the roles", sql).await
}

/// Drops roles (best effort for each).
pub(crate) async fn drop_roles(
    engine: &PostgresEngine,
    roles: &[EngineRole],
) -> Result<(), EngineError> {
    let mut admin = engine.admin().await?;
    let mut first_error = None;
    for role in roles {
        let sql = format!("drop role if exists {}", ident(&role.role_name));
        if let Err(e) = run(&mut admin, "drop a role", sql).await
            && first_error.is_none()
        {
            first_error = Some(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Makes an empty database that nobody but `ruum_dbhost` may connect to yet: created with
/// `ALLOW_CONNECTIONS false`, `CONNECT` and `TEMPORARY` revoked from `PUBLIC`, then opened.
/// With `template`, it is a copy of that database (the caller drained it).
pub(crate) async fn create_closed_database(
    engine: &PostgresEngine,
    name: &str,
    template: Option<&str>,
    open: bool,
) -> Result<(), EngineError> {
    let mut admin = engine.admin().await?;
    let template = template
        .map(|t| format!(" template {}", ident(t)))
        .unwrap_or_default();
    let create = format!(
        "create database {}{template} allow_connections false",
        ident(name)
    );
    let mut tries = 0;
    loop {
        match sqlx::raw_sql(sqlx::AssertSqlSafe(create.clone()))
            .execute(&mut *admin)
            .await
        {
            Ok(_) => break,
            // The source still has a session that is just ending (decision 0004-27).
            Err(e) if super::errors::sqlstate(&e).as_deref() == Some("55006") && tries < 20 => {
                tries += 1;
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
            Err(e) => return Err(internal("create the database", e)),
        }
    }
    let mut sql = format!(
        "revoke connect, temporary on database {db} from public;\n",
        db = ident(name)
    );
    if open {
        sql.push_str(&format!(
            "alter database {} allow_connections true;\n",
            ident(name)
        ));
    }
    run(&mut admin, "close the new database to PUBLIC", sql).await
}

/// Grants `CONNECT` on the database to its LOGIN roles (the last step before it is ready).
pub(crate) async fn grant_connect(
    engine: &PostgresEngine,
    name: &str,
    roles: &[EngineRole],
) -> Result<(), EngineError> {
    let logins: Vec<String> = roles
        .iter()
        .filter(|r| r.kind != RoleKind::Mask)
        .map(|r| ident(&r.role_name))
        .collect();
    let mut admin = engine.admin().await?;
    run(
        &mut admin,
        "grant connect",
        format!(
            "grant connect on database {} to {}",
            ident(name),
            logins.join(", ")
        ),
    )
    .await
}

/// The Ruum schemas, the schema `extensions` and their grants, in a new empty database
/// (decision 0004-34 b, g). `ruum_dbhost` owns every schema.
fn schemas_sql(roles: &[EngineRole], extensions: &[String]) -> String {
    let mut sql = String::from(
        "alter schema public owner to ruum_dbhost;\n\
         revoke all on schema public from public;\n\
         create schema ruum_meta;\n\
         create schema ruum_mask;\n\
         create schema ruum_masked;\n\
         create schema ruum_nosecret;\n\
         create schema extensions;\n\
         revoke all on schema ruum_meta, ruum_mask, ruum_masked, ruum_nosecret, extensions from public;\n\
         create table ruum_meta.ident (id int primary key check (id = 1), tenant_id uuid not null, \
           node_id uuid not null, state text not null check (state in ('live', 'snapshot')));\n\
         create table ruum_meta.roles (kind text not null, binding uuid, role_name text not null);\n\
         create table ruum_meta.restore_points (at timestamptz primary key, lsn text, \
           snapshot text not null);\n\
         create table ruum_meta.lens (id int primary key check (id = 1), spec jsonb not null);\n\
         revoke all on all tables in schema ruum_meta from public;\n",
    );
    for ext in extensions {
        sql.push_str(&format!(
            "create extension if not exists {} schema extensions;\n",
            ident(ext)
        ));
    }
    sql.push_str(&grants_sql(roles));
    sql
}

/// The schema grants of a database's roles.
pub(crate) fn grants_sql(roles: &[EngineRole]) -> String {
    let r = |k| ident(role_of(roles, k));
    let logins: Vec<String> = RoleKind::SESSION_KINDS.iter().map(|k| r(*k)).collect();
    format!(
        "grant usage, create on schema public to {owner};\n\
         grant usage on schema public to {secret_read}, {mask};\n\
         grant usage on schema ruum_mask, ruum_masked, ruum_nosecret to {mask};\n\
         grant usage on schema ruum_nosecret to {read}, {write};\n\
         grant usage on schema ruum_masked to {masked_read}, {masked_write};\n\
         grant usage on schema extensions to {logins}, {mask};\n",
        owner = r(RoleKind::Owner),
        secret_read = r(RoleKind::SecretRead),
        mask = r(RoleKind::Mask),
        read = r(RoleKind::Read),
        write = r(RoleKind::Write),
        masked_read = r(RoleKind::MaskedRead),
        masked_write = r(RoleKind::MaskedWrite),
        logins = logins.join(", "),
    )
}

/// The default privileges of the `owner` kind's tables and sequences: `mask` reads and writes
/// the base tables (an auto-updatable view needs its owner's rights), `secret_read` reads them,
/// and the kinds that insert through a view may use the sequences of defaults.
pub(crate) fn default_privileges_sql(roles: &[EngineRole], for_role: Option<&str>) -> String {
    let r = |k| ident(role_of(roles, k));
    let target = for_role
        .map(|o| format!(" for role {}", ident(o)))
        .unwrap_or_default();
    format!(
        "alter default privileges{target} in schema public grant select, insert, update, delete on tables to {mask};\n\
         alter default privileges{target} in schema public grant select on tables to {secret_read};\n\
         alter default privileges{target} in schema public grant usage, select on sequences to {mask}, {write}, {masked_write};\n",
        mask = r(RoleKind::Mask),
        secret_read = r(RoleKind::SecretRead),
        write = r(RoleKind::Write),
        masked_write = r(RoleKind::MaskedWrite),
    )
}

/// Writes the meta data of a database (in the caller's transaction).
pub(crate) async fn write_meta(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    node_id: Uuid,
    roles: &[EngineRole],
) -> Result<(), EngineError> {
    run(
        conn,
        "clear the meta data",
        "delete from ruum_meta.ident; delete from ruum_meta.roles; \
         delete from ruum_meta.restore_points"
            .to_string(),
    )
    .await?;
    sqlx::query(
        "insert into ruum_meta.ident (id, tenant_id, node_id, state) values (1, $1, $2, 'live')",
    )
    .persistent(false)
    .bind(tenant_id)
    .bind(node_id)
    .execute(&mut *conn)
    .await
    .map_err(|e| internal("write the meta data", e))?;
    for role in roles {
        sqlx::query("insert into ruum_meta.roles (kind, binding, role_name) values ($1, $2, $3)")
            .persistent(false)
            .bind(role.kind.as_str())
            .bind(role.binding)
            .bind(&role.role_name)
            .execute(&mut *conn)
            .await
            .map_err(|e| internal("write the roles", e))?;
    }
    Ok(())
}

/// Reads the meta data of a live database; `None` for a snapshot or a database without it.
pub(crate) async fn read_meta(conn: &mut PgConnection) -> Result<Option<Meta>, EngineError> {
    let ident = sqlx::query("select tenant_id, node_id, state from ruum_meta.ident where id = 1")
        .persistent(false)
        .fetch_optional(&mut *conn)
        .await;
    let ident = match ident {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(None),
        // No schema ruum_meta: not a database of this engine.
        Err(e) if super::errors::sqlstate(&e).as_deref() == Some("42P01") => return Ok(None),
        Err(e) => return Err(internal("read the meta data", e)),
    };
    if ident.get::<String, _>("state") != "live" {
        return Ok(None);
    }
    let roles = read_roles(conn).await?;
    Ok(Some(Meta {
        tenant_id: ident.get("tenant_id"),
        node_id: ident.get("node_id"),
        roles,
    }))
}

pub(crate) async fn read_roles(conn: &mut PgConnection) -> Result<Vec<EngineRole>, EngineError> {
    let rows = sqlx::query("select kind, binding, role_name from ruum_meta.roles")
        .persistent(false)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the roles", e))?;
    let mut roles = Vec::with_capacity(rows.len());
    for row in rows {
        let kind: String = row.get("kind");
        let kind = RoleKind::parse(&kind)
            .ok_or_else(|| EngineError::Internal(format!("unknown role kind {kind}")))?;
        roles.push(EngineRole {
            kind,
            binding: row.get("binding"),
            role_name: row.get("role_name"),
        });
    }
    roles.sort_by_key(|r| RoleKind::ALL.iter().position(|k| *k == r.kind));
    Ok(roles)
}

/// The lens spec kept in `ruum_meta.lens` (empty when there is none).
pub(crate) async fn read_lens_spec(conn: &mut PgConnection) -> Result<LensSpec, EngineError> {
    let row = sqlx::query("select spec from ruum_meta.lens where id = 1")
        .persistent(false)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| internal("read the lens spec", e))?;
    Ok(row
        .map(|r| lens::spec_from_json(&r.get::<serde_json::Value, _>("spec")))
        .unwrap_or_default())
}

async fn write_lens_spec(conn: &mut PgConnection, spec: &LensSpec) -> Result<(), EngineError> {
    sqlx::query(
        "insert into ruum_meta.lens (id, spec) values (1, $1) \
         on conflict (id) do update set spec = excluded.spec",
    )
    .persistent(false)
    .bind(lens::spec_to_json(spec))
    .execute(&mut *conn)
    .await
    .map_err(|e| internal("write the lens spec", e))?;
    Ok(())
}

/// Makes the key table and the pseudonym functions as `mask`, and installs `key` (in the
/// caller's transaction). `mask` holds `CREATE` on the lens schemas for this step only.
pub(crate) async fn install_functions(
    conn: &mut PgConnection,
    roles: &[EngineRole],
    key: &PseudonymKey,
) -> Result<(), EngineError> {
    let mask_name = role_of(roles, RoleKind::Mask);
    let mask = ident(mask_name);
    run(
        conn,
        "let mask create in the lens schemas",
        lens::mask_create_sql(mask_name, true),
    )
    .await?;
    run(conn, "become mask", format!("set local role {mask}")).await?;
    run(
        conn,
        "make the pseudonym functions",
        lens::functions_sql(
            role_of(roles, RoleKind::MaskedRead),
            role_of(roles, RoleKind::MaskedWrite),
        ),
    )
    .await?;
    install_key_as_mask(conn, key).await?;
    run(conn, "leave mask", "reset role".to_string()).await?;
    run(
        conn,
        "take CREATE in the lens schemas back from mask",
        lens::mask_create_sql(mask_name, false),
    )
    .await
}

/// Writes `key` into `ruum_mask.key`. The caller is `mask` already.
async fn install_key_as_mask(
    conn: &mut PgConnection,
    key: &PseudonymKey,
) -> Result<(), EngineError> {
    sqlx::query(lens::INSTALL_KEY_SQL)
        .persistent(false)
        .bind(key.expose().to_vec())
        .execute(&mut *conn)
        .await
        .map_err(|e| internal("install the key", e))?;
    Ok(())
}

/// Replaces the key of a database (in the caller's transaction).
pub(crate) async fn install_key(
    conn: &mut PgConnection,
    roles: &[EngineRole],
    key: &PseudonymKey,
) -> Result<(), EngineError> {
    let mask = ident(role_of(roles, RoleKind::Mask));
    run(conn, "become mask", format!("set local role {mask}")).await?;
    install_key_as_mask(conn, key).await?;
    run(conn, "leave mask", "reset role".to_string()).await
}

/// Drops the lens views (in the caller's transaction).
pub(crate) async fn drop_lens(
    conn: &mut PgConnection,
    roles: &[EngineRole],
) -> Result<(), EngineError> {
    let mask = ident(role_of(roles, RoleKind::Mask));
    run(conn, "become mask", format!("set local role {mask}")).await?;
    lens::drop_views(conn).await?;
    run(conn, "leave mask", "reset role".to_string()).await
}

/// Builds the lens views from `spec` and keeps the spec in `ruum_meta.lens` (in the caller's
/// transaction, after [`drop_lens`]).
pub(crate) async fn build_lens(
    conn: &mut PgConnection,
    roles: &[EngineRole],
    catalog: &Catalog,
    spec: &LensSpec,
) -> Result<(), EngineError> {
    write_lens_spec(conn, spec).await?;
    let mask_name = role_of(roles, RoleKind::Mask);
    let mask = ident(mask_name);
    run(
        conn,
        "let mask create in the lens schemas",
        lens::mask_create_sql(mask_name, true),
    )
    .await?;
    run(conn, "become mask", format!("set local role {mask}")).await?;
    lens::drop_views(conn).await?;
    let lens_roles = LensRoles {
        read: role_of(roles, RoleKind::Read),
        write: role_of(roles, RoleKind::Write),
        masked_read: role_of(roles, RoleKind::MaskedRead),
        masked_write: role_of(roles, RoleKind::MaskedWrite),
    };
    lens::build_views(conn, catalog, spec, &lens_roles).await?;
    run(conn, "leave mask", "reset role".to_string()).await?;
    run(
        conn,
        "take CREATE in the lens schemas back from mask",
        lens::mask_create_sql(mask_name, false),
    )
    .await
}

/// Reads the catalog, calls `sync` and rebuilds the lens views from its answer, in one
/// `ruum_dbhost` transaction (decision 0004-34 f). With `views_dropped`, the views are gone
/// already (a migration dropped them).
pub(crate) async fn sync_and_rebuild(
    engine: &PostgresEngine,
    state: &DbState,
    sync: &dyn CatalogSync,
    _views_dropped: bool,
) -> Result<Catalog, EngineError> {
    let mut conn = engine.dbhost(&state.name).await?;
    let result = async {
        run(&mut conn, "begin", "begin".to_string()).await?;
        let catalog = catalog::read(&mut conn).await?;
        let spec = sync
            .sync(&catalog)
            .await
            .map_err(|e| EngineError::CatalogSync(e.to_string()))?;
        build_lens(&mut conn, &state.meta.roles, &catalog, &spec).await?;
        run(&mut conn, "commit the lens", "commit".to_string()).await?;
        Ok(catalog)
    }
    .await;
    let _ = conn.close().await;
    result
}

/// Drops the lens views in their own transaction (before a migration).
pub(crate) async fn drop_lens_now(
    engine: &PostgresEngine,
    state: &DbState,
) -> Result<(), EngineError> {
    let mut conn = engine.dbhost(&state.name).await?;
    let result = async {
        run(&mut conn, "begin", "begin".to_string()).await?;
        drop_lens(&mut conn, &state.meta.roles).await?;
        run(&mut conn, "commit", "commit".to_string()).await
    }
    .await;
    let _ = conn.close().await;
    result
}

/// `create_database`: a new empty database with its roles, the Ruum schemas, the functions and
/// `key` installed.
pub(crate) async fn create_database(
    engine: &PostgresEngine,
    new: &NewDatabase,
    key: &PseudonymKey,
) -> Result<CreatedDatabase, EngineError> {
    let name = neutral_name("d_");
    let roles = new_roles();
    let made = async {
        create_closed_database(engine, &name, None, true).await?;
        create_roles(engine, &roles).await?;
        let mut conn = engine.dbhost(&name).await?;
        let result = async {
            run(&mut conn, "begin", "begin".to_string()).await?;
            run(
                &mut conn,
                "make the Ruum schemas",
                schemas_sql(&roles, &new.extensions),
            )
            .await?;
            write_meta(&mut conn, new.tenant_id, new.node_id, &roles).await?;
            install_functions(&mut conn, &roles, key).await?;
            build_lens(&mut conn, &roles, &Catalog::default(), &LensSpec::default()).await?;
            run(&mut conn, "commit", "commit".to_string()).await
        }
        .await;
        let _ = conn.close().await;
        result?;
        grant_connect(engine, &name, &roles).await?;
        let meta = Meta {
            tenant_id: new.tenant_id,
            node_id: new.node_id,
            roles: roles.clone(),
        };
        let state = Arc::new(DbState::new(name.clone(), meta));
        // The default privileges, once, in an `owner` session (decision 0004-34 b).
        let owner = engine.pool(&state, RoleKind::Owner);
        let mut lease = owner.acquire(engine).await?;
        let ran = run(
            lease.conn(),
            "set the default privileges",
            default_privileges_sql(&roles, None),
        )
        .await;
        match ran {
            Ok(()) => lease.release(engine).await,
            Err(e) => {
                lease.close().await;
                return Err(e);
            }
        }
        engine.remember(state);
        Ok::<(), EngineError>(())
    }
    .await;
    if let Err(e) = made {
        discard(engine, &name, &roles).await;
        return Err(e);
    }
    Ok(CreatedDatabase {
        database: DatabaseRef {
            tenant_id: new.tenant_id,
            node_id: new.node_id,
            cluster: engine.config.cluster.clone(),
            engine_name: name,
        },
        roles,
    })
}

/// Drops a database that was never handed out, and its roles (best effort).
pub(crate) async fn discard(engine: &PostgresEngine, name: &str, roles: &[EngineRole]) {
    engine.forget(name);
    if let Ok(mut admin) = engine.admin().await {
        let _ = run(
            &mut admin,
            "drop the unfinished database",
            format!("drop database if exists {} with (force)", ident(name)),
        )
        .await;
    }
    let _ = drop_roles(engine, roles).await;
}
