//! The lens of a Postgres database: the pseudonym functions in `ruum_mask` and the lens views
//! in `ruum_nosecret` and `ruum_masked` (`docs/STEP2.md` "The engine security model",
//! Postgres items 3 and 4; decisions 0004-4 and 0004-34 e).
//!
//! Everything here runs as `ruum_dbhost` under `SET LOCAL ROLE <mask>` in a transaction of the
//! caller: the key, the functions and the views are owned by the database's `mask` role.
//!
//! The input of a rebuild is the catalog and the classes that the catalog sync of the platform
//! answers ([`LensSpec`]: `coalesce(confirmed_class, 'personal')` per column; a column the spec
//! does not name counts as `personal`). Card Q-2 later feeds it from `database_columns`; this
//! file does not depend on `ruum-core`.
//!
//! # The pseudonym in SQL
//!
//! `ruum_mask.pseudonym(v) = 'p_' || first 12 hex of HMAC-SHA-256(key, canonical(v))`, with the
//! padded keys in `ruum_mask.key` (`ipad`, `opad`; the built-in `sha256()`, no extension). One
//! overload per type, each `SECURITY DEFINER`, owned by `mask`, with `search_path` set to
//! `pg_catalog, pg_temp`, and a `ruum_mask.canonical(<type>)` with fixed output settings, so no
//! session setting changes a pseudonym:
//!
//! | Overload | Canonical text |
//! |---|---|
//! | `text` (also `varchar`, `char(n)`, `name` through `::text`) | as is |
//! | `smallint`, `integer`, `bigint` | decimal (own overloads: the implicit cast of `int2` / `int4` picks `double precision`) |
//! | `numeric` | `trim_scale(v)::text` |
//! | `real`, `double precision` | `v::text` with `extra_float_digits = 1` (the `float4out` / `float8out` layout) |
//! | `timestamptz`, `timestamp` | `YYYY-MM-DDTHH:MM:SS[.ffffff]Z` in UTC |
//! | `date` | `YYYY-MM-DD` |
//! | `bytea` | lowercase hex |
//! | `boolean` | `true` / `false` |
//! | `uuid` | lowercase |
//!
//! **Infinite and BC times and dates** (review-Q-0 note 3): an infinite value, or a value
//! before `0001-01-01` (BC), takes the output text of its type with `DateStyle = ISO, YMD` and
//! `TimeZone = UTC`: `infinity`, `-infinity`, `0044-03-15 BC`, `0044-03-15 10:00:00+00 BC`,
//! `0044-03-15 10:00:00 BC`. The Rust side passes such a value as `SqlValue::Other` with
//! that text.
//!
//! Any other **built-in base type** (a type of `pg_catalog` with an OID below 16384 and
//! `typtype = 'b'`, arrays of such types included) goes through `ruum_mask.pseudonym_other`:
//! the type's output text (`v::text`) with every output setting fixed (`DateStyle = ISO, YMD`,
//! `IntervalStyle = postgres`, `TimeZone = UTC`, `extra_float_digits = 1`, `bytea_output =
//! hex`, `lc_monetary = C`), as the generic overload of `ruum_core::privacy`. It has its own
//! name, so that function resolution never has to choose between it and a typed overload.
//!
//! # Tenant-defined types (decision 0005 B-2, decision 0004-37)
//!
//! A `personal` column whose type is **not** a built-in base type (an enum, a domain, a
//! composite, a range or multirange, an array of one of those, a type of an extension) is masked
//! like `secret`: the masked view shows [`SECRET_CONSTANT`], never a pseudonym. The tenant may own
//! such a type and so may add casts, operators or functions for it; none of them may ever run
//! inside a `SECURITY DEFINER` function of `ruum_mask` (review-dbhost-2 item 2). So:
//!
//! - the lens calls a pseudonym function only for a column whose declared type is a built-in
//!   base type ([`pseudonym_types`]); a column of any other type, or one whose type is unknown,
//!   gets [`hidden_expr`], which converts nothing;
//! - `pseudonym_other` checks the type of its argument first and raises an error for any type
//!   that is not a built-in base type, before any conversion (a direct call, for example from
//!   tenant code in step 2 of a masked copy, cannot reach a tenant cast);
//! - every function has `search_path = pg_catalog, pg_temp`, and the bodies of `hmac12` and
//!   `pseudonym_other` qualify their functions and operators with `pg_catalog`;
//! - `mask` holds `USAGE` only on `ruum_mask`, `ruum_masked` and `ruum_nosecret`. `ruum_dbhost`
//!   (the owner of the schemas) grants it `CREATE` inside the transaction that makes the
//!   functions or builds the lens, and takes it back before the commit ([`mask_create_sql`]).
//!   This only blocks `CREATE OR REPLACE` and new objects. `mask` still owns the functions, the
//!   key and the views, and an owner needs no `CREATE` to replace the `_RETURN` rule of a view,
//!   to `ALTER FUNCTION`, to add a trigger or to grant rights (review-masking-1 F1,
//!   review-masking-2 F-2). The `CREATE` revoke is defence in depth only.
//!
//! **The protection is that no tenant code ever runs as `mask`** (the rules above). In addition,
//! every build of the lens (`setup::build_lens`: each migration, `sync_lens`, step 3 of a copy, a
//! restore in place) drops the three lens schemas and makes the functions, the key table (the
//! key is kept or replaced) and the views again, and takes back the default privileges of
//! `mask`. So a change that code as `mask` made, or the objects of the code from before a fix,
//! do not survive the next build. A build refuses to go on when an object outside the lens
//! schemas depends on an object inside them (`setup::foreign_lens_dependents`).
//!
//! The masked kinds have `EXECUTE` on `pseudonym` and `pseudonym_other` only (not on `hmac12`
//! or `canonical`), and no `USAGE` on `ruum_mask`: only a lens view (owner `mask`) calls them.

use super::errors::internal;
use super::values::oid;
use crate::error::EngineError;
use crate::model::*;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;

/// The constant a masked `secret` value shows (the query layer turns it into the hidden-value
/// marker). `NULL` stays `NULL`.
pub const SECRET_CONSTANT: &str = "ruum:hidden";

/// Quotes an identifier as `quote_ident` does (always quoted).
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A string literal (standard conforming strings).
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The typed overloads: (SQL type, canonical body). `$1` is the value.
const TYPED: &[(&str, &str, &str)] = &[
    // (type, extra function settings, canonical expression)
    ("text", "", "v"),
    ("smallint", "", "v::text"),
    ("integer", "", "v::text"),
    ("bigint", "", "v::text"),
    ("numeric", "", "pg_catalog.trim_scale(v)::text"),
    ("real", " set extra_float_digits = 1", "v::text"),
    ("double precision", " set extra_float_digits = 1", "v::text"),
    (
        "timestamptz",
        " set timezone = 'UTC' set datestyle = 'ISO, YMD'",
        "case when pg_catalog.isfinite(v) and v >= '0001-01-01 00:00:00+00'::timestamptz \
         then pg_catalog.rtrim(pg_catalog.rtrim(pg_catalog.to_char(v at time zone 'UTC', \
         'YYYY-MM-DD\"T\"HH24:MI:SS.US'), '0'), '.') || 'Z' else v::text end",
    ),
    (
        "timestamp",
        " set timezone = 'UTC' set datestyle = 'ISO, YMD'",
        "case when pg_catalog.isfinite(v) and v >= '0001-01-01 00:00:00'::timestamp \
         then pg_catalog.rtrim(pg_catalog.rtrim(pg_catalog.to_char(v, \
         'YYYY-MM-DD\"T\"HH24:MI:SS.US'), '0'), '.') || 'Z' else v::text end",
    ),
    (
        "date",
        " set timezone = 'UTC' set datestyle = 'ISO, YMD'",
        "case when pg_catalog.isfinite(v) and v >= '0001-01-01'::date \
         then pg_catalog.to_char(v, 'YYYY-MM-DD') else v::text end",
    ),
    ("bytea", "", "pg_catalog.encode(v, 'hex')"),
    ("boolean", "", "case when v then 'true' else 'false' end"),
    ("uuid", "", "v::text"),
];

/// The SQL condition on `pg_catalog.pg_type t` of a built-in base type: in `pg_catalog`, an OID
/// below `FirstNormalObjectId` (16384, so no type of the tenant or of an extension), and
/// `typtype = 'b'` (no domain, enum, composite, range, multirange or pseudo-type). A tenant
/// cannot add a cast between two such types: `CREATE CAST` needs the ownership of one of them.
pub(crate) const BUILT_IN_BASE: &str = "t.oid operator(pg_catalog.<) 16384::pg_catalog.oid \
     and t.typtype operator(pg_catalog.=) 'b' \
     and t.typnamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace";

/// The fixed output settings of `pseudonym_other`.
const OTHER_SETTINGS: &str = " set datestyle = 'ISO, YMD' set intervalstyle = 'postgres' \
     set timezone = 'UTC' set extra_float_digits = 1 set bytea_output = 'hex' \
     set lc_monetary = 'C'";

/// The SQL that makes `ruum_mask.key` and the functions. Run as the database's `mask` role.
/// The key itself is written by [`install_key_sql`].
pub(crate) fn functions_sql(masked_read: &str, masked_write: &str) -> String {
    let mut sql = String::from(
        "create table ruum_mask.key (id int primary key check (id = 1), \
           ipad bytea not null, opad bytea not null);\n\
         create function ruum_mask.hmac12(t text) returns text language sql stable strict \
           security definer set search_path = pg_catalog, pg_temp as $f$ \
           select 'p_' operator(pg_catalog.||) pg_catalog.left(pg_catalog.encode(pg_catalog.sha256( \
           k.opad operator(pg_catalog.||) pg_catalog.sha256(k.ipad operator(pg_catalog.||) \
           pg_catalog.convert_to(t, 'UTF8'))), 'hex'), 12) \
           from ruum_mask.key k where k.id operator(pg_catalog.=) 1 $f$;\n",
    );
    for (ty, settings, body) in TYPED {
        sql.push_str(&format!(
            "create function ruum_mask.canonical(v {ty}) returns text language sql stable strict \
               set search_path = pg_catalog, pg_temp{settings} as $f$ select {body} $f$;\n\
             create function ruum_mask.pseudonym(v {ty}) returns text language sql stable strict \
               security definer set search_path = pg_catalog, pg_temp as $f$ \
               select ruum_mask.hmac12(ruum_mask.canonical(v)) $f$;\n"
        ));
    }
    sql.push_str(&format!(
        "create function ruum_mask.pseudonym_other(v anyelement) returns text language plpgsql \
           stable strict security definer set search_path = pg_catalog, pg_temp{OTHER_SETTINGS} \
           as $f$ begin \
             if not exists (select 1 from pg_catalog.pg_type t \
                where t.oid operator(pg_catalog.=) pg_catalog.pg_typeof(v)::pg_catalog.oid \
                  and {BUILT_IN_BASE}) then \
               raise exception 'ruum_mask: no pseudonym for a type that is not a built-in base type' \
                 using errcode = '42804'; \
             end if; \
             return ruum_mask.hmac12(v::text); \
           end $f$;\n\
         revoke all on all functions in schema ruum_mask from public;\n\
         revoke all on table ruum_mask.key from public;\n"
    ));
    let grantees = format!("{}, {}", ident(masked_read), ident(masked_write));
    for (ty, _, _) in TYPED {
        sql.push_str(&format!(
            "grant execute on function ruum_mask.pseudonym({ty}) to {grantees};\n"
        ));
    }
    sql.push_str(&format!(
        "grant execute on function ruum_mask.pseudonym_other(anyelement) to {grantees};\n"
    ));
    sql
}

/// Replaces the key in `ruum_mask.key` with the padded keys of `$1` (32 bytes, a bind
/// parameter, so the key never appears in a statement text). Run as `mask`.
pub(crate) const INSTALL_KEY_SQL: &str = "
with k as (select $1::bytea || pg_catalog.decode(pg_catalog.repeat('00', 32), 'hex') as k),
     pads as (
       select pg_catalog.string_agg(pg_catalog.lpad(pg_catalog.to_hex(pg_catalog.get_byte(k.k, i) # 54), 2, '0'), '' order by i) as ipad,
              pg_catalog.string_agg(pg_catalog.lpad(pg_catalog.to_hex(pg_catalog.get_byte(k.k, i) # 92), 2, '0'), '' order by i) as opad
         from k, pg_catalog.generate_series(0, 63) i)
insert into ruum_mask.key (id, ipad, opad)
select 1, pg_catalog.decode(ipad, 'hex'), pg_catalog.decode(opad, 'hex') from pads
on conflict (id) do update set ipad = excluded.ipad, opad = excluded.opad";

/// The SQL call that pseudonymizes the column expression `expr` of the built-in base type
/// `base_oid`. The caller has checked the type with [`pseudonym_types`].
pub(crate) fn pseudonym_call(expr: &str, base_oid: u32) -> String {
    match base_oid {
        oid::TEXT => format!("ruum_mask.pseudonym({expr})"),
        oid::VARCHAR | oid::BPCHAR | oid::NAME | oid::CHAR => {
            format!("ruum_mask.pseudonym(({expr})::text)")
        }
        oid::INT2
        | oid::INT4
        | oid::INT8
        | oid::NUMERIC
        | oid::FLOAT4
        | oid::FLOAT8
        | oid::TIMESTAMPTZ
        | oid::TIMESTAMP
        | oid::DATE
        | oid::BYTEA
        | oid::BOOL
        | oid::UUID => {
            format!("ruum_mask.pseudonym({expr})")
        }
        _ => format!("ruum_mask.pseudonym_other({expr})"),
    }
}

/// The masked form of a `secret` value: `NULL` stays `NULL`, anything else is the constant.
/// The same form as [`hidden_expr`], so a secret composite whose fields are all `NULL` shows the
/// constant too (review-masking-2 F-3).
pub(crate) fn secret_expr(expr: &str) -> String {
    hidden_expr(expr)
}

/// The masked form of a value of a type that is not a built-in base type (decision 0005 B-2):
/// like `secret`, but the null test looks at the value itself (`num_nulls`), never inside it, so
/// a composite value whose fields are all `NULL` still shows the constant. Nothing of the type
/// (no cast, operator or function of the tenant) runs.
pub(crate) fn hidden_expr(expr: &str) -> String {
    format!(
        "case when pg_catalog.num_nulls({expr}) operator(pg_catalog.=) 1 then null else {}::text end",
        literal(SECRET_CONSTANT)
    )
}

/// The statement that gives `mask` (`grant: true`) or takes back `CREATE` on the lens schemas.
/// `ruum_dbhost` (their owner) runs it around the steps that make the functions or the views, in
/// the same transaction, so `mask` never holds `CREATE` after a commit.
pub(crate) fn mask_create_sql(mask: &str, grant: bool) -> String {
    let (verb, to) = if grant {
        ("grant", "to")
    } else {
        ("revoke", "from")
    };
    format!(
        "{verb} create on schema ruum_mask, ruum_masked, ruum_nosecret {to} {}",
        ident(mask)
    )
}

/// The columns of the relations in `public` that may get a pseudonym: (rel_oid, attnum) -> OID
/// of the declared type, for a column whose declared type is a built-in base type
/// ([`BUILT_IN_BASE`]). A column of any other type (a domain too) is not in the map and is
/// masked like `secret`.
pub(crate) async fn pseudonym_types(
    conn: &mut PgConnection,
) -> Result<HashMap<(u32, i16), u32>, EngineError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "select a.attrelid::pg_catalog.int8 as rel, a.attnum::pg_catalog.int2 as attnum,
                a.atttypid::pg_catalog.int8 as typ
           from pg_catalog.pg_attribute a
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) a.attrelid
           join pg_catalog.pg_type t on t.oid operator(pg_catalog.=) a.atttypid
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and a.attnum operator(pg_catalog.>) 0 and not a.attisdropped
            and {BUILT_IN_BASE}"
    )))
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("read the column types", e))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let rel: i64 = r.get("rel");
            let typ: i64 = r.get("typ");
            ((rel as u32, r.get::<i16, _>("attnum")), typ as u32)
        })
        .collect())
}

/// The roles of the lens grants.
pub(crate) struct LensRoles<'a> {
    pub read: &'a str,
    pub write: &'a str,
    pub masked_read: &'a str,
    pub masked_write: &'a str,
}

/// The SQL of the lens views of one relation of `public`, with their grants. Run as `mask`.
pub(crate) fn view_sql(
    table: &CatalogTable,
    spec: &LensSpec,
    pseudonym_types: &HashMap<(u32, i16), u32>,
    roles: &LensRoles<'_>,
) -> String {
    let mut nosecret = Vec::with_capacity(table.columns.len());
    let mut masked = Vec::with_capacity(table.columns.len());
    for col in &table.columns {
        let name = ident(&col.name);
        let class = spec.class_of(&ColumnRef {
            schema: table.schema.clone(),
            table: table.name.clone(),
            column: col.name.clone(),
        });
        // `None`: not a built-in base type, or unknown. Masked like `secret` (0005 B-2).
        let pseudonym_type = col
            .identity
            .and_then(|i| pseudonym_types.get(&(i.rel_oid, i.attnum)).copied());
        match class {
            DataClass::Normal => {
                nosecret.push(name.clone());
                masked.push(name);
            }
            DataClass::Personal => {
                nosecret.push(name.clone());
                match pseudonym_type {
                    Some(t) => masked.push(format!("{} as {name}", pseudonym_call(&name, t))),
                    None => masked.push(format!("{} as {name}", hidden_expr(&name))),
                }
            }
            DataClass::Secret => {
                nosecret.push(format!("{} as {name}", secret_expr(&name)));
                masked.push(format!("{} as {name}", secret_expr(&name)));
            }
        }
    }
    let source = format!("public.{}", ident(&table.name));
    let view = ident(&table.name);
    format!(
        "create view ruum_nosecret.{view} as select {} from {source};\n\
         grant select on ruum_nosecret.{view} to {};\n\
         grant select, insert, update, delete on ruum_nosecret.{view} to {};\n\
         create view ruum_masked.{view} as select {} from {source};\n\
         grant select on ruum_masked.{view} to {};\n\
         grant select, insert, update, delete on ruum_masked.{view} to {};\n",
        nosecret.join(", "),
        ident(roles.read),
        ident(roles.write),
        masked.join(", "),
        ident(roles.masked_read),
        ident(roles.masked_write),
    )
}

/// Drops every lens view. Run as `mask` in the caller's transaction.
pub(crate) async fn drop_views(conn: &mut PgConnection) -> Result<(), EngineError> {
    let rows = sqlx::query(
        "select pg_catalog.format('drop view if exists %I.%I cascade', n.nspname, c.relname) as sql
           from pg_catalog.pg_class c join pg_catalog.pg_namespace n on n.oid = c.relnamespace
          where n.nspname in ('ruum_masked', 'ruum_nosecret') and c.relkind = 'v'",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("list the lens views", e))?;
    for r in rows {
        let sql: String = r.get("sql");
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *conn)
            .await
            .map_err(|e| internal("drop a lens view", e))?;
    }
    Ok(())
}

/// Builds the lens views of every relation of `public` in `catalog`. Run as `mask` in the
/// caller's transaction, after [`drop_views`].
pub(crate) async fn build_views(
    conn: &mut PgConnection,
    catalog: &Catalog,
    spec: &LensSpec,
    roles: &LensRoles<'_>,
) -> Result<(), EngineError> {
    let bases = pseudonym_types(conn).await?;
    for table in catalog.tables.iter().filter(|t| t.schema == "public") {
        let sql = view_sql(table, spec, &bases, roles);
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *conn)
            .await
            .map_err(|e| internal("build a lens view", e))?;
    }
    Ok(())
}

/// A [`LensSpec`] as JSON, kept in `ruum_meta.lens` so that a copy rebuilds the same lens.
pub(crate) fn spec_to_json(spec: &LensSpec) -> serde_json::Value {
    serde_json::Value::Array(
        spec.columns
            .iter()
            .map(|(c, class)| serde_json::json!([c.schema, c.table, c.column, class]))
            .collect(),
    )
}

pub(crate) fn spec_from_json(value: &serde_json::Value) -> LensSpec {
    let columns = value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let schema = item.get(0)?.as_str()?.to_string();
                    let table = item.get(1)?.as_str()?.to_string();
                    let column = item.get(2)?.as_str()?.to_string();
                    let class: DataClass = serde_json::from_value(item.get(3)?.clone()).ok()?;
                    Some((
                        ColumnRef {
                            schema,
                            table,
                            column,
                        },
                        class,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    LensSpec { columns }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_literals_are_quoted() {
        assert_eq!(ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(literal("it's"), "'it''s'");
    }

    #[test]
    fn the_overload_follows_the_base_type() {
        assert_eq!(
            pseudonym_call("\"a\"", oid::INT4),
            "ruum_mask.pseudonym(\"a\")"
        );
        assert_eq!(
            pseudonym_call("\"a\"", oid::VARCHAR),
            "ruum_mask.pseudonym((\"a\")::text)"
        );
        assert_eq!(
            pseudonym_call("\"a\"", 1186),
            "ruum_mask.pseudonym_other(\"a\")"
        );
    }

    #[test]
    fn a_view_masks_by_class_and_a_missing_column_is_personal() {
        let table = CatalogTable {
            schema: "public".into(),
            name: "users".into(),
            kind: RelationKind::Table,
            columns: ["id", "email", "token"]
                .iter()
                .enumerate()
                .map(|(i, n)| CatalogColumn {
                    name: n.to_string(),
                    data_type: "text".into(),
                    nullable: true,
                    identity: Some(ColumnIdentity {
                        rel_oid: 1,
                        attnum: i as i16 + 1,
                        type_oid: oid::TEXT,
                    }),
                })
                .collect(),
            primary_key: vec![],
            foreign_keys: vec![],
        };
        let col = |c: &str| ColumnRef {
            schema: "public".into(),
            table: "users".into(),
            column: c.into(),
        };
        let spec = LensSpec {
            columns: vec![
                (col("id"), DataClass::Normal),
                (col("token"), DataClass::Secret),
            ],
        };
        let bases = HashMap::from([
            ((1, 1), oid::TEXT),
            ((1, 2), oid::TEXT),
            ((1, 3), oid::TEXT),
        ]);
        let roles = LensRoles {
            read: "r_1",
            write: "r_2",
            masked_read: "r_3",
            masked_write: "r_4",
        };
        let sql = view_sql(&table, &spec, &bases, &roles);
        assert!(sql.contains(
            "create view ruum_masked.\"users\" as select \"id\", ruum_mask.pseudonym(\"email\") as \"email\", case when pg_catalog.num_nulls(\"token\") operator(pg_catalog.=) 1 then null"
        ), "{sql}");
        assert!(sql.contains(
            "create view ruum_nosecret.\"users\" as select \"id\", \"email\", case when pg_catalog.num_nulls(\"token\")"
        ), "{sql}");
        let back = spec_from_json(&spec_to_json(&spec));
        assert_eq!(back, spec);
    }

    /// Decision 0005 B-2: a personal column whose type is not a built-in base type (not in the
    /// map of `pseudonym_types`), or whose identity is unknown, is masked like `secret` and
    /// never reaches a pseudonym function.
    #[test]
    fn a_personal_column_of_a_tenant_type_is_hidden_not_pseudonymized() {
        let column = |name: &str, attnum: Option<i16>| CatalogColumn {
            name: name.to_string(),
            data_type: "mood".into(),
            nullable: true,
            identity: attnum.map(|a| ColumnIdentity {
                rel_oid: 7,
                attnum: a,
                type_oid: 90001,
            }),
        };
        let table = CatalogTable {
            schema: "public".into(),
            name: "t".into(),
            kind: RelationKind::Table,
            columns: vec![
                column("ok", Some(1)),
                column("mood", Some(2)),
                column("lost", None),
            ],
            primary_key: vec![],
            foreign_keys: vec![],
        };
        let spec = LensSpec { columns: vec![] };
        let types = HashMap::from([((7, 1), oid::TEXT)]);
        let roles = LensRoles {
            read: "r_1",
            write: "r_2",
            masked_read: "r_3",
            masked_write: "r_4",
        };
        let sql = view_sql(&table, &spec, &types, &roles);
        let masked = sql
            .lines()
            .find(|l| l.starts_with("create view ruum_masked."))
            .unwrap();
        assert!(
            masked.contains("ruum_mask.pseudonym(\"ok\") as \"ok\""),
            "{masked}"
        );
        assert!(
            masked.contains(&format!("{} as \"mood\"", hidden_expr("\"mood\""))),
            "{masked}"
        );
        assert!(
            masked.contains(&format!("{} as \"lost\"", hidden_expr("\"lost\""))),
            "{masked}"
        );
        assert_eq!(masked.matches("ruum_mask.").count(), 1, "{masked}");
    }

    /// `pseudonym_other` refuses a type that is not a built-in base type before it converts,
    /// and the definer bodies resolve nothing outside `pg_catalog`.
    #[test]
    fn the_definer_functions_check_the_type_first() {
        let sql = functions_sql("r_3", "r_4");
        let other = &sql[sql
            .find("create function ruum_mask.pseudonym_other")
            .unwrap()..];
        let check = other.find("raise exception").unwrap();
        let convert = other.find("v::text").unwrap();
        assert!(check < convert, "{other}");
        assert!(other.contains(BUILT_IN_BASE), "{other}");
        let hmac = &sql[sql.find("create function ruum_mask.hmac12").unwrap()..];
        let hmac = &hmac[..hmac.find("$f$;").unwrap()];
        assert!(!hmac.contains(" || "), "{hmac}");
        assert!(hmac.contains("pg_catalog.sha256"), "{hmac}");
        assert_eq!(
            mask_create_sql("r_9", false),
            "revoke create on schema ruum_mask, ruum_masked, ruum_nosecret from \"r_9\""
        );
    }
}
