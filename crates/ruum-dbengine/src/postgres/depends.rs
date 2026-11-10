//! What the computed columns of `public` depend on (card Q-2; review-Q-2-1 F-1, F-3, F-4, F-5):
//! the views and materialized views (their `_RETURN` rule) and the generated columns (their
//! expression in `pg_attrdef`).
//!
//! From each such *root*, the walk follows `pg_depend` to the objects the root uses, and on to
//! what those use: the columns and relations a rule reads, the functions and operators it calls,
//! the functions those call (a function with a SQL-standard body, `RETURN ...` or `BEGIN
//! ATOMIC`, records what it calls; an operator records its function; an aggregate its
//! transition function), and the `_RETURN` rule of every view it reaches. Only objects of the
//! tenant (OID 16384 and above) are followed. The walk does not go on from a table.
//!
//! Two users:
//!
//! - [`definitions`]: a hash of everything the walk reaches (rule trees, function bodies and
//!   settings, operator functions, generated expressions), per column. The catalog puts it into
//!   [`ColumnIdentity::definition`](crate::model::ColumnIdentity), so a view replaced in place
//!   (same OID and `attnum`), a generated expression changed in place, or a function replaced in
//!   place that a view calls, clears the confirmation of the column (review-Q-2-1 F-1).
//! - [`root_facts`]: what the lens needs to decide whether a root reads a `secret` value
//!   (`lens::secret_reading`): the columns and whole relations it reads, whether it reaches a
//!   `SECURITY DEFINER` function of the tenant (also through other functions, F-4), and whether a
//!   rule it reaches holds a whole-row reference (`to_json(t)`, `t::text`: a `Var` with
//!   `varattno 0`, which `pg_depend` records only as the whole relation, F-3).
//!
//! Every name and operator is qualified with `pg_catalog`: this runs as `mask` (search path
//! `pg_catalog`) and in the `owner` session of a migration.

use super::errors::internal;
use crate::error::EngineError;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;

/// The recursive walk (see the module docs): `reach(rel, att, classid, objid, subid)`, where
/// `(rel, att)` is the root (`att` 0 for a view) and `subid` is the `refobjsubid` of the edge
/// (the column of a relation, 0 for the whole relation or another object).
const REACH: &str = r#"
with recursive roots(rel, att, classid, objid) as (
  select v.oid, 0::pg_catalog.int2,
         'pg_catalog.pg_rewrite'::pg_catalog.regclass::pg_catalog.oid, r.oid
    from pg_catalog.pg_class v
    join pg_catalog.pg_rewrite r
      on r.ev_class operator(pg_catalog.=) v.oid
     and r.rulename operator(pg_catalog.=) '_RETURN'
   where v.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
     and v.relkind operator(pg_catalog.=) any ('{v,m}'::pg_catalog."char"[])
  union all
  select a.attrelid, a.attnum,
         'pg_catalog.pg_attrdef'::pg_catalog.regclass::pg_catalog.oid, ad.oid
    from pg_catalog.pg_attribute a
    join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) a.attrelid
    join pg_catalog.pg_attrdef ad
      on ad.adrelid operator(pg_catalog.=) a.attrelid
     and ad.adnum operator(pg_catalog.=) a.attnum
   where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
     and a.attgenerated operator(pg_catalog.<>) ''::pg_catalog."char"
     and not a.attisdropped
),
reach(rel, att, classid, objid, subid) as (
  select rel, att, classid, objid, 0::pg_catalog.int4 from roots
  union
  select reach.rel, reach.att, e.classid, e.objid, e.subid
    from reach
    cross join lateral (
      select d.refclassid as classid, d.refobjid as objid, d.refobjsubid as subid
        from pg_catalog.pg_depend d
       where d.classid operator(pg_catalog.=) reach.classid
         and d.objid operator(pg_catalog.=) reach.objid
         and reach.classid operator(pg_catalog.<>) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid
         and d.refobjid operator(pg_catalog.>=) 16384::pg_catalog.oid
         and d.refclassid operator(pg_catalog.=) any (array[
               'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid,
               'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid,
               'pg_catalog.pg_operator'::pg_catalog.regclass::pg_catalog.oid])
      union all
      select 'pg_catalog.pg_rewrite'::pg_catalog.regclass::pg_catalog.oid, r.oid, 0
        from pg_catalog.pg_rewrite r
       where reach.classid operator(pg_catalog.=) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid
         and r.ev_class operator(pg_catalog.=) reach.objid
         and r.rulename operator(pg_catalog.=) '_RETURN'
    ) e
)"#;

/// The definition hash of each root: `(rel, att)` -> hash (see the module docs).
pub(crate) async fn definitions(
    conn: &mut PgConnection,
) -> Result<HashMap<(u32, i16), i64>, EngineError> {
    let sql = format!(
        r#"{REACH},
defs as (
  select reach.rel, reach.att,
         case
           when reach.classid operator(pg_catalog.=) 'pg_catalog.pg_rewrite'::pg_catalog.regclass::pg_catalog.oid
             then (select r.ev_action::pg_catalog.text from pg_catalog.pg_rewrite r
                    where r.oid operator(pg_catalog.=) reach.objid)
           when reach.classid operator(pg_catalog.=) 'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid
             then (select pg_catalog.concat_ws('|', p.prosrc, p.prosqlbody::pg_catalog.text,
                          p.prosecdef::pg_catalog.text, p.provolatile::pg_catalog.text,
                          p.proconfig::pg_catalog.text, p.prolang::pg_catalog.text,
                          p.prokind::pg_catalog.text, p.probin)
                     from pg_catalog.pg_proc p where p.oid operator(pg_catalog.=) reach.objid)
           when reach.classid operator(pg_catalog.=) 'pg_catalog.pg_operator'::pg_catalog.regclass::pg_catalog.oid
             then (select o.oprcode::pg_catalog.oid::pg_catalog.text from pg_catalog.pg_operator o
                    where o.oid operator(pg_catalog.=) reach.objid)
           when reach.classid operator(pg_catalog.=) 'pg_catalog.pg_attrdef'::pg_catalog.regclass::pg_catalog.oid
             then (select ad.adbin::pg_catalog.text from pg_catalog.pg_attrdef ad
                    where ad.oid operator(pg_catalog.=) reach.objid)
         end as def
    from reach
)
select rel::pg_catalog.int8 as rel, att::pg_catalog.int2 as att,
       ('x' operator(pg_catalog.||) pg_catalog.left(pg_catalog.md5(
          pg_catalog.string_agg(def, ',' order by def)), 16))::pg_catalog.bit(64)::pg_catalog.int8 as hash
  from defs
 where def is not null
 group by rel, att"#
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the definitions of the computed columns", e))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let rel: i64 = r.get("rel");
            (
                (rel as u32, r.get::<i16, _>("att")),
                r.get::<i64, _>("hash"),
            )
        })
        .collect())
}

/// What one root reads (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RootFacts {
    /// The relation of the root (a view, or the table of a generated column).
    pub rel: u32,
    /// 0 for a view (every column), else the `attnum` of a generated column.
    pub att: i16,
    /// The columns it reads: (relation, `attnum`).
    pub columns: Vec<(u32, i16)>,
    /// The relations it names as a whole (every relation of a rule's range table).
    pub whole_relations: Vec<u32>,
    /// It reaches a `SECURITY DEFINER` function outside `pg_catalog`.
    pub definer: bool,
    /// A rule it reaches holds a whole-row reference.
    pub whole_row: bool,
}

/// The [`RootFacts`] of every root of `public`.
pub(crate) async fn root_facts(conn: &mut PgConnection) -> Result<Vec<RootFacts>, EngineError> {
    let sql = format!(
        r#"{REACH}
select reach.rel::pg_catalog.int8 as rel, reach.att::pg_catalog.int2 as att,
       (reach.classid operator(pg_catalog.=) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid) as is_rel,
       reach.objid::pg_catalog.int8 as obj, reach.subid::pg_catalog.int4 as sub,
       (reach.classid operator(pg_catalog.=) 'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid
        and exists (select 1 from pg_catalog.pg_proc p
                     where p.oid operator(pg_catalog.=) reach.objid and p.prosecdef
                       and p.pronamespace operator(pg_catalog.<>) 'pg_catalog'::pg_catalog.regnamespace)) as definer,
       (reach.classid operator(pg_catalog.=) 'pg_catalog.pg_rewrite'::pg_catalog.regclass::pg_catalog.oid
        and exists (select 1 from pg_catalog.pg_rewrite r
                     where r.oid operator(pg_catalog.=) reach.objid
                       and pg_catalog.strpos(r.ev_action::pg_catalog.text, ':varattno 0 ')
                           operator(pg_catalog.>) 0)) as whole_row
  from reach"#
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the dependencies of the computed columns", e))?;
    let mut by_root: HashMap<(u32, i16), RootFacts> = HashMap::new();
    for r in rows {
        let rel = r.get::<i64, _>("rel") as u32;
        let att: i16 = r.get("att");
        let f = by_root.entry((rel, att)).or_insert_with(|| RootFacts {
            rel,
            att,
            ..RootFacts::default()
        });
        if r.get::<bool, _>("is_rel") {
            let obj = r.get::<i64, _>("obj") as u32;
            match r.get::<i32, _>("sub") {
                0 => f.whole_relations.push(obj),
                sub => f.columns.push((obj, sub as i16)),
            }
        }
        f.definer |= r.get::<bool, _>("definer");
        f.whole_row |= r.get::<bool, _>("whole_row");
    }
    let mut out: Vec<RootFacts> = by_root.into_values().collect();
    out.sort_by_key(|f| (f.rel, f.att));
    Ok(out)
}

/// Whether a write in `public` can run tenant code that sees the real row: an enabled trigger
/// that is not internal (a foreign key's triggers are internal), or a rule other than a view's
/// `_RETURN` (review-Q-2-2 P-1). Such code runs on the base table with the real `NEW` and
/// `OLD`, and can copy a value into a column that the writer may read.
pub(crate) const SIDE_EFFECTS_SQL: &str = "
select exists (
         select 1 from pg_catalog.pg_trigger t
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) t.tgrelid
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and not t.tgisinternal
            and t.tgenabled operator(pg_catalog.<>) 'D'::pg_catalog.\"char\")
    or exists (
         select 1 from pg_catalog.pg_rewrite r
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) r.ev_class
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and r.rulename operator(pg_catalog.<>) '_RETURN'
            and r.ev_enabled operator(pg_catalog.<>) 'D'::pg_catalog.\"char\") as side_effects";

/// [`SIDE_EFFECTS_SQL`] in the session of `conn`.
pub(crate) async fn write_side_effects(conn: &mut PgConnection) -> Result<bool, EngineError> {
    let row = sqlx::query(SIDE_EFFECTS_SQL)
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| internal("read the triggers and rules", e))?;
    Ok(row.get("side_effects"))
}
