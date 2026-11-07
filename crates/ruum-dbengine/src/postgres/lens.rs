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
//! Any other type goes through `ruum_mask.pseudonym_other(anyelement)`: the type's output text
//! (`v::text`) with every output setting fixed (`DateStyle = ISO, YMD`, `IntervalStyle =
//! postgres`, `TimeZone = UTC`, `extra_float_digits = 1`, `bytea_output = hex`, `lc_monetary =
//! C`), as the generic overload of `ruum_core::privacy`. It has its own name, so that function
//! resolution never has to choose between it and a typed overload. A domain uses the overload of
//! its base type.
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
           select 'p_' || left(encode(sha256(k.opad || sha256(k.ipad || convert_to(t, 'UTF8'))), \
           'hex'), 12) from ruum_mask.key k where k.id = 1 $f$;\n",
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
        "create function ruum_mask.pseudonym_other(v anyelement) returns text language sql \
           stable strict security definer set search_path = pg_catalog, pg_temp{OTHER_SETTINGS} \
           as $f$ select ruum_mask.hmac12(v::text) $f$;\n\
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

/// The SQL call that pseudonymizes the column expression `expr` of base type `base_oid`.
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
pub(crate) fn secret_expr(expr: &str) -> String {
    format!(
        "case when {expr} is null then null else {}::text end",
        literal(SECRET_CONSTANT)
    )
}

/// The base type of every column of the relations in `public`: (rel_oid, attnum) -> OID of
/// the base type (a domain resolved to the type under it).
pub(crate) async fn base_types(
    conn: &mut PgConnection,
) -> Result<HashMap<(u32, i16), u32>, EngineError> {
    let rows = sqlx::query(
        "with recursive d(rel, attnum, typ, depth) as (
           select a.attrelid, a.attnum, a.atttypid, 0
             from pg_catalog.pg_attribute a
             join pg_catalog.pg_class c on c.oid = a.attrelid
            where c.relnamespace = 'public'::pg_catalog.regnamespace
              and a.attnum > 0 and not a.attisdropped
           union all
           select d.rel, d.attnum, t.typbasetype, d.depth + 1
             from d join pg_catalog.pg_type t on t.oid = d.typ
            where t.typtype = 'd' and d.depth < 32)
         select distinct on (rel, attnum) rel::int8 as rel, attnum::int2 as attnum, typ::int8 as typ
           from d order by rel, attnum, depth desc",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("read the base types", e))?;
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
    base_types: &HashMap<(u32, i16), u32>,
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
        let base = col
            .identity
            .and_then(|i| base_types.get(&(i.rel_oid, i.attnum)).copied())
            .unwrap_or(0);
        match class {
            DataClass::Normal => {
                nosecret.push(name.clone());
                masked.push(name);
            }
            DataClass::Personal => {
                nosecret.push(name.clone());
                masked.push(format!("{} as {name}", pseudonym_call(&name, base)));
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
    let bases = base_types(conn).await?;
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
            "create view ruum_masked.\"users\" as select \"id\", ruum_mask.pseudonym(\"email\") as \"email\", case when \"token\" is null"
        ), "{sql}");
        assert!(sql.contains(
            "create view ruum_nosecret.\"users\" as select \"id\", \"email\", case when \"token\""
        ), "{sql}");
        let back = spec_from_json(&spec_to_json(&spec));
        assert_eq!(back, spec);
    }
}
