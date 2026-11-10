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
//!   `varattno 0`, which `pg_depend` records only as the whole relation, F-3), and whether it
//!   reaches a function outside the allow-list (`lens::code_relations`, [`super::allow`];
//!   review-Q-2-s21-2 R-1 and R-2, review-Q-2-harden H-1).
//!
//! Every name and operator is qualified with `pg_catalog`: this runs as `mask` (search path
//! `pg_catalog`) and in the `owner` session of a migration.

use super::allow;
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
    /// It reaches a function or operator outside the allow-list ([`allow`]): a function of the
    /// tenant or of an extension, or a built-in function that is not on the list of functions
    /// that may run on the real rows (a session setting, a lock, a random seed, a sequence, a
    /// large object). A masked read of the view would run it on the real rows
    /// (review-Q-2-s21-2 R-1 and R-2, review-Q-2-harden H-1).
    pub unlisted: bool,
}

/// The [`RootFacts`] of every root of `public`.
pub(crate) async fn root_facts(conn: &mut PgConnection) -> Result<Vec<RootFacts>, EngineError> {
    let rule_unlisted = allow::unlisted("r.ev_action::pg_catalog.text");
    let default_unlisted = allow::unlisted("ad.adbin::pg_catalog.text");
    let allowed = allow::allowed_cte(1);
    let sql = format!(
        r#"{REACH},
{allowed}
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
                           operator(pg_catalog.>) 0)) as whole_row,
       (
         -- A function or operator of the tenant or of an extension that the walk reached (the
         -- walk follows only objects of OID 16384 and above; built-ins are pinned).
         reach.classid operator(pg_catalog.=) any (array[
              'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid,
              'pg_catalog.pg_operator'::pg_catalog.regclass::pg_catalog.oid])
         -- A built-in function or operator in the stored tree of a rule that the walk reached.
         or (reach.classid operator(pg_catalog.=) 'pg_catalog.pg_rewrite'::pg_catalog.regclass::pg_catalog.oid
             and exists (select 1 from pg_catalog.pg_rewrite r
                          where r.oid operator(pg_catalog.=) reach.objid and {rule_unlisted}))
         or (reach.classid operator(pg_catalog.=) 'pg_catalog.pg_attrdef'::pg_catalog.regclass::pg_catalog.oid
             and exists (select 1 from pg_catalog.pg_attrdef ad
                          where ad.oid operator(pg_catalog.=) reach.objid and {default_unlisted}))
       ) as unlisted
  from reach"#
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .bind(allow::row_functions())
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
        f.unlisted |= r.get::<bool, _>("unlisted");
    }
    let mut out: Vec<RootFacts> = by_root.into_values().collect();
    out.sort_by_key(|f| (f.rel, f.att));
    Ok(out)
}

/// Whether the database holds **tenant code**: any object of the tenant that Postgres may run
/// during a statement and that sees the real rows, whatever lens the session has (decision
/// 0004-38, design 8.5 "Tenant code and masked sessions"; review-Q-2-2 P-1, review-Q-2-3 R-1).
///
/// The tenant's objects are those owned by the database's `owner` role (`$1`) or in a schema
/// that it may create in (today `public` only), so a later grant cannot open a gap. Each clause
/// is one kind:
///
/// - `functions`: any function, procedure or aggregate of the tenant, of any language,
///   volatility or security. A `STABLE` function can call a `VOLATILE` one, so volatility tells
///   nothing. Casts, operators, aggregates, column defaults, generated expressions, domain
///   checks and range types all need such a function to run tenant code, so this clause covers
///   them. Functions of the allow-listed extensions are in schema `extensions`, which the
///   tenant does not own and cannot create in.
/// - `triggers`: every trigger that is not internal (a foreign key's triggers are internal),
///   enabled or not (an `ENABLE TRIGGER` is DDL with the same race as a new trigger). A
///   trigger can call a built-in function (`tsvector_update_trigger`), so this is not covered
///   by `functions`.
/// - `rules`: every rule but a view's own `_RETURN` (`ON SELECT`), enabled or not.
/// - `policies`: row-level security policies; their expressions run on the real rows.
/// - `relations`: a relation of a kind outside the known list (`r` table, `p` partitioned
///   table, `v` view, `m` materialized view, `S` sequence, `i` and `I` indexes, `c` composite
///   type, `t` TOAST). A foreign table (`f`) sends a write to another server, and an unknown
///   kind counts as tenant code (fail closed).
/// - `event_triggers`: owned by the tenant (they need a superuser today; listed so that a later
///   grant cannot open a gap).
/// - `unlisted_functions`: code that runs on the real rows and calls a built-in function outside
///   the allow-list ([`allow`]): the `_RETURN` rule of a view, a `CHECK` constraint (also of a
///   domain), an index expression or predicate, a generated column. Built-in functions are
///   pinned, so `pg_depend` cannot see them; the clause reads the stored node trees. A column
///   default is not scanned: it sees no row. A function of the tenant is in `functions`; a
///   policy is in `policies`.
///
/// `$1` is the role name of the `owner` kind, `$2` the names of [`allow::ROW_FUNCTIONS`].
/// Every name and operator is qualified: this runs in the session of an untrusted request too,
/// whose search path is a lens schema.
pub(crate) fn tenant_code_sql() -> String {
    const TEMPLATE: &str = "
with owner as (
  select r.oid from pg_catalog.pg_roles r where r.rolname operator(pg_catalog.=) $1
), tenant as (
  select n.oid from pg_catalog.pg_namespace n
   where n.nspowner operator(pg_catalog.=) (select oid from owner)
      or pg_catalog.has_schema_privilege($1, n.oid, 'CREATE')
), @ALLOWED@
select exists (
         select 1 from pg_catalog.pg_proc p
          where p.pronamespace operator(pg_catalog.=) any (select oid from tenant)
             or p.proowner operator(pg_catalog.=) (select oid from owner)) as functions,
       exists (
         select 1 from pg_catalog.pg_trigger t
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) t.tgrelid
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and not t.tgisinternal) as triggers,
       exists (
         select 1 from pg_catalog.pg_rewrite r
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) r.ev_class
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and not (r.rulename operator(pg_catalog.=) '_RETURN'
                     and r.ev_type operator(pg_catalog.=) '1'::pg_catalog.\"char\")) as rules,
       exists (
         select 1 from pg_catalog.pg_policy y
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) y.polrelid
          where c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
             or c.relowner operator(pg_catalog.=) (select oid from owner)) as policies,
       exists (
         select 1 from pg_catalog.pg_class c
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and c.relkind operator(pg_catalog.<>) all
                ('{r,p,v,m,S,i,I,c,t}'::pg_catalog.\"char\"[])) as relations,
       exists (
         select 1 from pg_catalog.pg_event_trigger e
          where e.evtowner operator(pg_catalog.=) (select oid from owner)) as event_triggers,
       exists (
         select 1 from pg_catalog.pg_rewrite r
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) r.ev_class
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and c.relkind operator(pg_catalog.=) 'v'::pg_catalog.\"char\"
            and r.rulename operator(pg_catalog.=) '_RETURN'
            and @RULE@) as unlisted_views,
       exists (
         select 1 from pg_catalog.pg_constraint k
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) k.conrelid
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and k.contype operator(pg_catalog.=) 'c'::pg_catalog.\"char\"
            and @CHECK@) as unlisted_checks,
       exists (
         select 1 from pg_catalog.pg_constraint k
           join pg_catalog.pg_type t on t.oid operator(pg_catalog.=) k.contypid
          where (t.typnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or t.typowner operator(pg_catalog.=) (select oid from owner))
            and k.contype operator(pg_catalog.=) 'c'::pg_catalog.\"char\"
            and @CHECK@) as unlisted_domains,
       exists (
         select 1 from pg_catalog.pg_index i
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) i.indrelid
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and @INDEX@) as unlisted_indexes,
       exists (
         select 1 from pg_catalog.pg_attrdef ad
           join pg_catalog.pg_attribute a
             on a.attrelid operator(pg_catalog.=) ad.adrelid and a.attnum operator(pg_catalog.=) ad.adnum
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) ad.adrelid
          where (c.relnamespace operator(pg_catalog.=) any (select oid from tenant)
                 or c.relowner operator(pg_catalog.=) (select oid from owner))
            and a.attgenerated operator(pg_catalog.<>) ''::pg_catalog.\"char\"
            and @GENERATED@) as unlisted_generated";
    TEMPLATE
        .replace("@ALLOWED@", &allow::allowed_cte(2))
        .replace("@RULE@", &allow::unlisted("r.ev_action::pg_catalog.text"))
        .replace("@CHECK@", &allow::unlisted("k.conbin::pg_catalog.text"))
        .replace(
            "@INDEX@",
            &allow::unlisted(
                "pg_catalog.concat_ws(' ', i.indexprs::pg_catalog.text, i.indpred::pg_catalog.text)",
            ),
        )
        .replace("@GENERATED@", &allow::unlisted("ad.adbin::pg_catalog.text"))
}

/// The kinds of tenant code that [`tenant_code_sql`] found (all `false`: none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TenantCode {
    pub functions: bool,
    pub triggers: bool,
    pub rules: bool,
    pub policies: bool,
    pub relations: bool,
    pub event_triggers: bool,
    /// Code that runs on the real rows and calls a function outside the allow-list.
    pub unlisted_functions: bool,
}

impl TenantCode {
    /// Whether any kind was found.
    pub fn any(self) -> bool {
        self.functions
            || self.triggers
            || self.rules
            || self.policies
            || self.relations
            || self.event_triggers
            || self.unlisted_functions
    }
}

/// [`tenant_code_sql`] in the session of `conn`; `owner` is the role name of the database's
/// `owner` kind.
pub(crate) async fn tenant_code(
    conn: &mut PgConnection,
    owner: &str,
) -> Result<TenantCode, EngineError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(tenant_code_sql()))
        .persistent(false)
        .bind(owner)
        .bind(allow::row_functions())
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| internal("read the tenant code", e))?;
    Ok(TenantCode {
        functions: row.get("functions"),
        triggers: row.get("triggers"),
        rules: row.get("rules"),
        policies: row.get("policies"),
        relations: row.get("relations"),
        event_triggers: row.get("event_triggers"),
        unlisted_functions: ["views", "checks", "domains", "indexes", "generated"]
            .into_iter()
            .any(|kind| row.get::<bool, _>(format!("unlisted_{kind}").as_str())),
    })
}
