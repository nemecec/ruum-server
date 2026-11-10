//! The catalog of a database with column identity (`rel_oid`, `attnum`, `type_oid`, and the
//! definition hash of a computed column, `depends::definitions`; decision 0004: classes follow
//! identity), without the Ruum schemas (`ruum_*`), the schema
//! `extensions`, the system schemas and the objects of extensions.
//!
//! It runs in any session of the database (`ruum_dbhost`, or the `owner` session of a
//! migration inside its transaction), so every name is qualified with `pg_catalog`.

use super::errors::internal;
use crate::error::EngineError;
use crate::model::*;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;

const RELATIONS: &str = "
select c.oid::int8 as oid, n.nspname::text as schema, c.relname::text as name,
       c.relkind::text as kind
  from pg_catalog.pg_class c
  join pg_catalog.pg_namespace n on n.oid = c.relnamespace
 where c.relkind in ('r', 'p', 'v', 'm')
   and n.nspname not in ('pg_catalog', 'information_schema', 'extensions')
   and n.nspname !~ '^pg_'
   and n.nspname !~ '^ruum_'
   and not exists (select 1 from pg_catalog.pg_depend d
                    where d.classid = 'pg_catalog.pg_class'::pg_catalog.regclass
                      and d.objid = c.oid and d.deptype = 'e')
 order by n.nspname, c.relname";

const COLUMNS: &str = "
select a.attrelid::int8 as rel, a.attnum::int2 as attnum, a.attname::text as name,
       pg_catalog.format_type(a.atttypid, a.atttypmod) as data_type,
       a.attnotnull as notnull, a.atttypid::int8 as type_oid
  from pg_catalog.pg_attribute a
 where a.attrelid = any($1::pg_catalog.oid[]) and a.attnum > 0 and not a.attisdropped
 order by a.attrelid, a.attnum";

const PRIMARY_KEYS: &str = "
select c.conrelid::int8 as rel,
       pg_catalog.array_agg(a.attname::text order by k.ord) as cols
  from pg_catalog.pg_constraint c
 cross join lateral pg_catalog.unnest(c.conkey) with ordinality k(attnum, ord)
  join pg_catalog.pg_attribute a on a.attrelid = c.conrelid and a.attnum = k.attnum
 where c.contype = 'p' and c.conrelid = any($1::pg_catalog.oid[])
 group by c.conrelid";

const FOREIGN_KEYS: &str = "
select c.conrelid::int8 as rel, c.conname::text as name,
       (select pg_catalog.array_agg(a.attname::text order by k.ord)
          from pg_catalog.unnest(c.conkey) with ordinality k(attnum, ord)
          join pg_catalog.pg_attribute a on a.attrelid = c.conrelid and a.attnum = k.attnum) as cols,
       rn.nspname::text as ref_schema, r.relname::text as ref_table,
       (select pg_catalog.array_agg(a.attname::text order by k.ord)
          from pg_catalog.unnest(c.confkey) with ordinality k(attnum, ord)
          join pg_catalog.pg_attribute a on a.attrelid = c.confrelid and a.attnum = k.attnum) as ref_cols
  from pg_catalog.pg_constraint c
  join pg_catalog.pg_class r on r.oid = c.confrelid
  join pg_catalog.pg_namespace rn on rn.oid = r.relnamespace
 where c.contype = 'f' and c.conrelid = any($1::pg_catalog.oid[])
 order by c.conrelid, c.conname";

/// Reads the catalog in the session of `conn` (inside its transaction, if one is open).
pub(crate) async fn read(conn: &mut PgConnection) -> Result<Catalog, EngineError> {
    let rels = sqlx::query(RELATIONS)
        .persistent(false)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the relations", e))?;
    let mut tables: BTreeMap<i64, CatalogTable> = BTreeMap::new();
    let mut order: Vec<i64> = Vec::with_capacity(rels.len());
    for r in &rels {
        let oid: i64 = r.get("oid");
        let kind = match r.get::<String, _>("kind").as_str() {
            "v" => RelationKind::View,
            "m" => RelationKind::MaterializedView,
            _ => RelationKind::Table,
        };
        order.push(oid);
        tables.insert(
            oid,
            CatalogTable {
                schema: r.get("schema"),
                name: r.get("name"),
                kind,
                columns: Vec::new(),
                primary_key: Vec::new(),
                foreign_keys: Vec::new(),
            },
        );
    }
    if order.is_empty() {
        return Ok(Catalog::default());
    }
    let oids: Vec<i64> = order.clone();
    // What the computed columns compute (review-Q-2-1 F-1): a view's hash for each of its
    // columns, a generated column's own hash.
    let definitions = super::depends::definitions(&mut *conn).await?;
    for r in sqlx::query(COLUMNS)
        .persistent(false)
        .bind(&oids)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the columns", e))?
    {
        let rel: i64 = r.get("rel");
        if let Some(t) = tables.get_mut(&rel) {
            let notnull: bool = r.get("notnull");
            let type_oid: i64 = r.get("type_oid");
            let attnum: i16 = r.get("attnum");
            let definition = definitions
                .get(&(rel as u32, 0))
                .or_else(|| definitions.get(&(rel as u32, attnum)))
                .copied();
            t.columns.push(CatalogColumn {
                name: r.get("name"),
                data_type: r.get("data_type"),
                nullable: !notnull,
                identity: Some(ColumnIdentity {
                    rel_oid: rel as u32,
                    attnum,
                    type_oid: type_oid as u32,
                    definition,
                }),
            });
        }
    }
    for r in sqlx::query(PRIMARY_KEYS)
        .persistent(false)
        .bind(&oids)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the primary keys", e))?
    {
        let rel: i64 = r.get("rel");
        if let Some(t) = tables.get_mut(&rel) {
            t.primary_key = r.get("cols");
        }
    }
    for r in sqlx::query(FOREIGN_KEYS)
        .persistent(false)
        .bind(&oids)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the foreign keys", e))?
    {
        let rel: i64 = r.get("rel");
        if let Some(t) = tables.get_mut(&rel) {
            t.foreign_keys.push(ForeignKey {
                columns: r.get("cols"),
                references_schema: r.get("ref_schema"),
                references_table: r.get("ref_table"),
                references_columns: r.get("ref_cols"),
            });
        }
    }
    Ok(Catalog {
        tables: order
            .into_iter()
            .filter_map(|oid| tables.remove(&oid))
            .collect(),
    })
}
