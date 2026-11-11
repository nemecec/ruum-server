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
//!
//! # Computed columns over `secret` values (card Q-2, review-Q-1 N-9, review-Q-2-1 F-3 to F-5)
//!
//! A view or a materialized view of the tenant reads the base tables with the rights of its owner
//! (the `owner` kind), so its lens view would show what the tenant view computes from a `secret`
//! column: as a pseudonym to the masked kinds, and as it is to the `nosecret` kinds while the
//! view column counts as `personal` (a new column is unconfirmed). A generated column is the same
//! on a table: its stored value is computed from other columns. So a column of a tenant view, or
//! a generated column, whose class is `personal` (confirmed or not) is masked like `secret` when
//! it reads a `secret` value ([`secret_reading`], from `depends::root_facts`):
//!
//! - it reads a column whose class is `secret` (`pg_depend` of the view's `_RETURN` rule or of
//!   the generated expression, also through other views);
//! - it reaches a `SECURITY DEFINER` function of the tenant, directly or through other functions
//!   or operators (review-Q-2-1 F-4): such a function reads any table with the rights of its
//!   owner, and `pg_depend` does not show what it reads. (A function without `SECURITY DEFINER`
//!   runs as the reader, who has no right on the base tables and no `USAGE` on `public`; a
//!   function with a SQL-standard body records what it calls, so the walk follows it.);
//! - a rule it reaches holds a whole-row reference (`to_json(t)`, `t::text`) and it names a
//!   relation that has a `secret` column (review-Q-2-1 F-3). `pg_depend` records no column for
//!   a whole-row reference (a whole-relation entry, which Postgres drops when the rule reads a
//!   column of the same relation too), so any relation the root names counts. This over-masks
//!   a view with a whole-row reference to another relation next to such a relation (fail
//!   safe).
//!
//! `pg_depend` names the columns that a view reads, not which output column reads which, so every
//! `personal` column of such a view is masked like `secret` (fail safe). A column that a person
//! confirmed as `normal` keeps its class: the confirmation was made for what the column computes,
//! and a change of that (a view replaced in place, a generated expression or a function changed
//! in place) changes the column's definition hash and clears the confirmation (catalog sync,
//! `ColumnIdentity::definition`, review-Q-2-1 F-1).
//!
//! # Code on the real rows: the function allow-list (decision 0004-38, review-Q-2-harden)
//!
//! A tenant view runs with the rights of its owner on the real rows, and a table with a policy
//! runs the policy there. A function with an effect that outlives the expression (a session
//! setting, a lock, a random seed, a sequence, a large object) can carry a whole `secret` out of
//! such code into the same statement. So a relation whose view or policy reaches any function
//! outside the allow-list ([`super::allow`]: named built-in functions that cannot write, the
//! functions of built-in operators and casts) is **not part of the masked lens**: both lens
//! schemas hold a *refusal view* of the same name in its place ([`refusal_view_sql`]), and a
//! statement that reads it gets `statement_not_allowed` with the hint `tenant_code_on_live`. The
//! same holds for every view that reads such a relation ([`code_relations`]).
//!
//! Two more places run code on the real values, and the allow-list covers both (review
//! Q-2-harden-2 F-1 and F-2): a cast to a domain runs the checks of the domain (a view or a
//! policy that coerces to a domain with a check outside the list is outside the list, also when
//! the domain is nested in another type), and an index calls the support functions and operators
//! of its operator class on the raw keys (a table or materialized view with such an index, and
//! every view that reads it, is refused: [`index_relations`]).

//!
//! # Tenant views over masked inputs (decision 0004-39, review-Q-2-harden-3 F-3 and F-4)
//!
//! The allow-list decides which functions code on the real rows may call. It cannot decide what
//! the code does with a value: `generate_series(1, secret)` is allowed, and the row count carries
//! the secret. So **masked sessions only ever see tenant views evaluated over masked inputs**: the
//! lens view of a tenant view is the view's own definition with its base relations replaced by the
//! lens relations of the same schema (personal columns already pseudonyms, secret and
//! tenant-type columns already the hidden marker), with the lens masking by class on top
//! ([`rebuild_view`], [`rebuilt_view_sql`]). Views are built in dependency order ([`build_order`]),
//! so a view over a view is built over the lens view of the inner one. A view that cannot be built
//! over the lens (Postgres refuses the definition over text pseudonyms, it reads a system catalog
//! or a relation outside the lens, or it reaches anything outside the allow-list) gets the refusal
//! view, per lens. A policy runs on the table that a lens view reads and cannot be rebuilt; a table
//! whose policy reads a column that is not `normal` gets the refusal view ([`code_relations`]).

use super::allow;
use super::errors::internal;
use super::values::oid;
use crate::error::EngineError;
use crate::model::*;
use sqlx::{PgConnection, Row};
use std::collections::{HashMap, HashSet};

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

/// The two lens schemas of the masked kinds: `ruum_nosecret` (the kinds `read` and `write`: a
/// personal value as it is, a secret hidden) and `ruum_masked` (`masked_read` and `masked_write`:
/// a personal value as a pseudonym, a secret hidden).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lens {
    NoSecret,
    Masked,
}

impl Lens {
    pub(crate) const BOTH: [Lens; 2] = [Lens::NoSecret, Lens::Masked];

    pub(crate) fn schema(self) -> &'static str {
        match self {
            Lens::NoSecret => "ruum_nosecret",
            Lens::Masked => "ruum_masked",
        }
    }

    /// The grants of a view of this lens: `select` for the reading kind, all four for the writing
    /// kind.
    fn grants(self, view: &str, roles: &LensRoles<'_>) -> String {
        let (read, write) = match self {
            Lens::NoSecret => (roles.read, roles.write),
            Lens::Masked => (roles.masked_read, roles.masked_write),
        };
        format!(
            "grant select on {schema}.{view} to {read};\n\
             grant select, insert, update, delete on {schema}.{view} to {write};\n",
            schema = self.schema(),
            read = ident(read),
            write = ident(write),
        )
    }
}

/// The select lists of the two lens views of one relation of `public`: `(nosecret, masked)`.
/// Each item is a column of the relation (or of the subquery that stands for it).
pub(crate) fn lens_columns(
    table: &CatalogTable,
    spec: &LensSpec,
    pseudonym_types: &HashMap<(u32, i16), u32>,
    secret_reading: &HashSet<(u32, i16)>,
) -> (Vec<String>, Vec<String>) {
    let mut nosecret = Vec::with_capacity(table.columns.len());
    let mut masked = Vec::with_capacity(table.columns.len());
    for col in &table.columns {
        let name = ident(&col.name);
        // A column of a view, or a generated column, that reads a secret (module docs).
        let reads_secret = col.identity.is_some_and(|i| {
            secret_reading.contains(&(i.rel_oid, 0))
                || secret_reading.contains(&(i.rel_oid, i.attnum))
        });
        let class = match spec.class_of(&ColumnRef {
            schema: table.schema.clone(),
            table: table.name.clone(),
            column: col.name.clone(),
        }) {
            // A computed column that reads a secret value (see the module docs).
            DataClass::Personal if reads_secret => DataClass::Secret,
            other => other,
        };
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
    (nosecret, masked)
}

/// The SQL of the lens views of one relation of `public`, with their grants. Run as `mask`.
/// The views read the relation itself (a table, a materialized view, or a view whose lens is not
/// rebuilt: [`rebuilt_view_sql`]).
pub(crate) fn view_sql(
    table: &CatalogTable,
    spec: &LensSpec,
    pseudonym_types: &HashMap<(u32, i16), u32>,
    secret_reading: &HashSet<(u32, i16)>,
    roles: &LensRoles<'_>,
) -> String {
    let (nosecret, masked) = lens_columns(table, spec, pseudonym_types, secret_reading);
    let source = format!("public.{}", ident(&table.name));
    let view = ident(&table.name);
    format!(
        "create view ruum_nosecret.{view} as select {} from {source};\n{}\
         create view ruum_masked.{view} as select {} from {source};\n{}",
        nosecret.join(", "),
        Lens::NoSecret.grants(&view, roles),
        masked.join(", "),
        Lens::Masked.grants(&view, roles),
    )
}

/// The SQL that makes the lens view of a tenant view over the lens relations
/// ([`rebuild_view`], decision 0004-39): the select list of the lens `columns` over the
/// definition of the view, whose base relations are the relations of the same lens schema. The
/// view's own logic (joins, filters, expressions, row-producing functions) therefore runs on the
/// values that the lens shows, never on the real ones. `definition` is the text of
/// `pg_get_viewdef`; the statement is made with `search_path = <lens>, public, pg_catalog`.
pub(crate) fn rebuilt_view_sql(
    lens: Lens,
    table: &CatalogTable,
    columns: &[String],
    definition: &str,
) -> String {
    let body = definition.trim().trim_end_matches(';').trim();
    format!(
        "create view {}.{} as select {} from ({body}) as \"q\"",
        lens.schema(),
        ident(&table.name),
        columns.join(", "),
    )
}

/// The relations and columns of `public` that are masked like `secret` when their class is
/// `personal` (see the module docs): `(view, 0)` for every column of a view, `(table, attnum)`
/// for a generated column. A root counts when it reads a column whose class in `spec` is
/// `secret`, reaches a `SECURITY DEFINER` function of the tenant, or holds a whole-row reference
/// while it names a relation that has a `secret` column. A column that the catalog does not name
/// (another schema) does not count.
pub(crate) fn secret_reading(
    catalog: &Catalog,
    spec: &LensSpec,
    facts: &[super::depends::RootFacts],
) -> HashSet<(u32, i16)> {
    let mut secret_columns: HashSet<(u32, i16)> = HashSet::new();
    let mut secret_relations: HashSet<u32> = HashSet::new();
    for t in &catalog.tables {
        for c in &t.columns {
            let Some(i) = c.identity else { continue };
            let class = spec.class_of(&ColumnRef {
                schema: t.schema.clone(),
                table: t.name.clone(),
                column: c.name.clone(),
            });
            if class == DataClass::Secret {
                secret_columns.insert((i.rel_oid, i.attnum));
                secret_relations.insert(i.rel_oid);
            }
        }
    }
    facts
        .iter()
        .filter(|f| {
            // Postgres keeps the column entries of a relation and drops its whole-relation entry
            // when a rule reads columns of it too, so every relation the root names counts.
            let names_secret_relation = || {
                f.whole_relations
                    .iter()
                    .chain(f.columns.iter().map(|(r, _)| r))
                    .any(|r| secret_relations.contains(r))
            };
            f.definer
                || f.columns.iter().any(|c| secret_columns.contains(c))
                || (f.whole_row && names_secret_relation())
        })
        .map(|f| (f.rel, f.att))
        .collect()
}

/// The text that the planner raises when a statement reads a refusal view
/// ([`refusal_view_sql`]): the input of a cast to `int4`, so the error is `22P02` with this text
/// in quotes. The engine turns it into `statement_not_allowed` (`tenant_code_on_live`).
pub const REFUSAL_TEXT: &str = "ruum:tenant_code_on_live";

/// What one policy of a table of `public` reaches (see [`policy_edges`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyFact {
    /// A function or operator outside the allow-list, or a coercion to a type outside it.
    Outside,
    /// A relation that the policy names. `column` is the `attnum` of a column it reads, or 0 for
    /// the relation as such (the table of the policy itself, a relation in a sub-select).
    Reads { rel: u32, column: i16 },
    /// A whole-row reference (`p::text`, `to_json(p)`): `pg_depend` records no column for it.
    WholeRow,
}

/// What the policies of the tables of `public` reach: `(table, fact)`. Run as `mask` in the
/// caller's transaction.
///
/// A policy of the tenant runs on the real rows when the lens view reads the table, and it
/// cannot be rebuilt over the lens (decision 0004-39: Postgres applies it to the table that the
/// lens view reads). Built-in functions are pinned, so `pg_depend` has no row for them: the
/// first part of the query reads the stored tree of each policy ([`allow::unlisted`]); the
/// second part is the dependency rows (a relation, with the column that the policy reads, or a
/// function or operator of the tenant or of an extension); the third part finds the whole-row
/// references.
pub(crate) async fn policy_edges(
    conn: &mut PgConnection,
) -> Result<Vec<(u32, PolicyFact)>, EngineError> {
    let allowed = allow::ctes(1);
    let unlisted = allow::unlisted(
        "pg_catalog.concat_ws(' ', y.polqual::pg_catalog.text, y.polwithcheck::pg_catalog.text)",
    );
    let sql = format!(
        "with recursive {allowed}
         select y.polrelid::pg_catalog.int8 as rel, 'outside'::pg_catalog.text as kind,
                null::pg_catalog.int8 as names, 0::pg_catalog.int4 as sub
           from pg_catalog.pg_policy y
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) y.polrelid
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and {unlisted}
         union all
         select y.polrelid::pg_catalog.int8 as rel,
                case when d.refclassid operator(pg_catalog.=) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid
                     then 'reads' else 'outside' end as kind,
                case when d.refclassid operator(pg_catalog.=) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid
                     then d.refobjid::pg_catalog.int8 end as names,
                d.refobjsubid::pg_catalog.int4 as sub
           from pg_catalog.pg_policy y
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) y.polrelid
           join pg_catalog.pg_depend d
             on d.classid operator(pg_catalog.=) 'pg_catalog.pg_policy'::pg_catalog.regclass::pg_catalog.oid
            and d.objid operator(pg_catalog.=) y.oid
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and d.refobjid operator(pg_catalog.>=) 16384::pg_catalog.oid
            and (d.refclassid operator(pg_catalog.=) 'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid
                 or d.refclassid operator(pg_catalog.=) 'pg_catalog.pg_operator'::pg_catalog.regclass::pg_catalog.oid
                 or d.refclassid operator(pg_catalog.=) 'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid)
         union all
         select y.polrelid::pg_catalog.int8 as rel, 'whole_row'::pg_catalog.text as kind,
                null::pg_catalog.int8 as names, 0::pg_catalog.int4 as sub
           from pg_catalog.pg_policy y
           join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) y.polrelid
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and pg_catalog.strpos(
                  pg_catalog.concat_ws(' ', y.polqual::pg_catalog.text, y.polwithcheck::pg_catalog.text),
                  ':varattno 0 ') operator(pg_catalog.>) 0"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .bind(allow::row_functions())
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the policies", e))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let rel = r.get::<i64, _>("rel") as u32;
            let names: Option<i64> = r.get("names");
            let sub: i32 = r.get("sub");
            let fact = match (r.get::<String, _>("kind").as_str(), names) {
                ("reads", Some(n)) => PolicyFact::Reads {
                    rel: n as u32,
                    column: sub as i16,
                },
                ("whole_row", _) => PolicyFact::WholeRow,
                _ => PolicyFact::Outside,
            };
            (rel, fact)
        })
        .collect())
}

/// The relations of `public` with an index whose operator class holds a support function or an
/// operator outside the allow-list (review-Q-2-harden-2 F-2; [`allow::unlisted_index_support`]),
/// and the relations that inherit from or are partitions of them: a scan of the index, or of the
/// parent that reads it, calls the function on the raw keys. Run as `mask` in the caller's
/// transaction.
pub(crate) async fn index_relations(conn: &mut PgConnection) -> Result<Vec<u32>, EngineError> {
    let ctes = allow::ctes(1);
    let unlisted = allow::unlisted_index_support("i");
    let sql = format!(
        "with recursive {ctes},
         bad(rel) as (
           select i.indrelid from pg_catalog.pg_index i
             join pg_catalog.pg_class c on c.oid operator(pg_catalog.=) i.indrelid
            where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
              and {unlisted}
           union
           select h.inhparent from pg_catalog.pg_inherits h
             join bad on bad.rel operator(pg_catalog.=) h.inhrelid
         )
         select rel::pg_catalog.int8 as rel from bad"
    );
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .bind(allow::row_functions())
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| internal("read the index support functions", e))?;
    Ok(rows
        .into_iter()
        .map(|r| r.get::<i64, _>("rel") as u32)
        .collect())
}

/// The relations of `public` whose lens view must be a refusal view, because their read would run
/// code outside the allow-list on the real rows (review-Q-2-s21-2 R-1 and R-2, review-Q-2-harden
/// H-1), or a policy on the real rows that reads a restricted value (decision 0004-39):
///
/// - a view that reaches such a function ([`allow`]; `facts.unlisted`);
/// - a table with a policy that reaches one (the lens view reads the table as `mask`, so its
///   policies run on the real rows);
/// - a table whose policy reads a column that is not `normal`, or a view, or holds a whole-row
///   reference next to a relation with such a column. Postgres applies a policy to the table that
///   the lens view reads, so a policy cannot be evaluated over the lens (a tenant view can:
///   [`rebuild_view`]); a policy over `normal` columns of plain tables keeps working;
/// - a table or materialized view whose index would call code outside the list on the raw keys
///   (`indexes`, [`index_relations`]);
/// - every view or policy table that reads such a relation.
///
/// A materialized view runs nothing when it is read. `edges` are the [`policy_edges`].
pub(crate) fn code_relations(
    catalog: &Catalog,
    spec: &LensSpec,
    facts: &[super::depends::RootFacts],
    edges: &[(u32, PolicyFact)],
    indexes: &[u32],
) -> HashSet<u32> {
    let rel_of = |t: &CatalogTable| t.columns.iter().find_map(|c| c.identity).map(|i| i.rel_oid);
    let views: HashSet<u32> = catalog
        .tables
        .iter()
        .filter(|t| t.kind == RelationKind::View)
        .filter_map(rel_of)
        .collect();
    // The columns a person or the platform classed `normal`; every relation with any other
    // column; every relation the catalog names.
    let mut normal: HashSet<(u32, i16)> = HashSet::new();
    let mut restricted: HashSet<u32> = HashSet::new();
    let mut known: HashSet<u32> = HashSet::new();
    for t in &catalog.tables {
        for c in &t.columns {
            let Some(i) = c.identity else { continue };
            known.insert(i.rel_oid);
            let class = spec.class_of(&ColumnRef {
                schema: t.schema.clone(),
                table: t.name.clone(),
                column: c.name.clone(),
            });
            if class == DataClass::Normal {
                normal.insert((i.rel_oid, i.attnum));
            } else {
                restricted.insert(i.rel_oid);
            }
        }
    }
    let view_facts: Vec<_> = facts
        .iter()
        .filter(|f| f.att == 0 && views.contains(&f.rel))
        .collect();
    let mut code: HashSet<u32> = view_facts
        .iter()
        .filter(|f| f.unlisted)
        .map(|f| f.rel)
        .chain(indexes.iter().copied())
        .collect();
    let mut whole_row: HashSet<u32> = HashSet::new();
    for (rel, fact) in edges {
        match fact {
            PolicyFact::Outside => {
                code.insert(*rel);
            }
            PolicyFact::Reads { rel: read, column } => {
                // A column that is not known to be `normal`, or a view (its logic would run on
                // the real rows): the table cannot be read through the lens.
                if (*column > 0 && !normal.contains(&(*read, *column))) || views.contains(read) {
                    code.insert(*rel);
                }
            }
            PolicyFact::WholeRow => {
                whole_row.insert(*rel);
            }
        }
    }
    for rel in &whole_row {
        let mut read = vec![*rel];
        read.extend(edges.iter().filter_map(|(r, f)| match f {
            PolicyFact::Reads { rel: n, .. } if r == rel => Some(*n),
            _ => None,
        }));
        if read
            .iter()
            .any(|r| restricted.contains(r) || !known.contains(r))
        {
            code.insert(*rel);
        }
    }
    loop {
        let before = code.len();
        for f in &view_facts {
            let reads_code = f
                .whole_relations
                .iter()
                .chain(f.columns.iter().map(|(r, _)| r))
                .any(|r| code.contains(r));
            if reads_code {
                code.insert(f.rel);
            }
        }
        for (rel, fact) in edges {
            if matches!(fact, PolicyFact::Reads { rel: n, .. } if code.contains(n)) {
                code.insert(*rel);
            }
        }
        if code.len() == before {
            return code;
        }
    }
}

/// The declared type of each column of `rels`, as `format_type` writes it with the search path
/// of `mask` (`pg_catalog`), so a type of the tenant is qualified. Run as `mask`.
async fn column_types(
    conn: &mut PgConnection,
    rels: &HashSet<u32>,
) -> Result<HashMap<(u32, i16), String>, EngineError> {
    if rels.is_empty() {
        return Ok(HashMap::new());
    }
    let oids: Vec<i64> = rels.iter().map(|r| *r as i64).collect();
    let rows = sqlx::query(
        "select a.attrelid::pg_catalog.int8 as rel, a.attnum::pg_catalog.int2 as attnum,
                pg_catalog.format_type(a.atttypid, a.atttypmod) as typ
           from pg_catalog.pg_attribute a
          where a.attrelid operator(pg_catalog.=) any ($1::pg_catalog.int8[]::pg_catalog.oid[])
            and a.attnum operator(pg_catalog.>) 0 and not a.attisdropped",
    )
    .persistent(false)
    .bind(&oids)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("read the column types of the refused relations", e))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let rel: i64 = r.get("rel");
            ((rel as u32, r.get::<i16, _>("attnum")), r.get("typ"))
        })
        .collect())
}

/// The lens view of `lens` for a relation that the lens cannot show ([`code_relations`], or a
/// tenant view that cannot be rebuilt over the lens: [`rebuild_view`]): the same name, columns
/// and types, but the view never reads the relation. Planning any statement that reads it raises
/// `22P02` with [`REFUSAL_TEXT`] (a cast of a constant, folded by the planner, in the `FROM`
/// clause so that no statement can leave it out). Nothing of the tenant runs. Run as `mask`.
pub(crate) fn refusal_lens_sql(
    lens: Lens,
    table: &CatalogTable,
    types: &HashMap<(u32, i16), String>,
    roles: &LensRoles<'_>,
) -> String {
    let columns: Vec<String> = table
        .columns
        .iter()
        .map(|c| {
            let ty = c
                .identity
                .and_then(|i| types.get(&(i.rel_oid, i.attnum)))
                .map_or("pg_catalog.text", String::as_str);
            format!("null::{ty} as {}", ident(&c.name))
        })
        .collect();
    let body = format!(
        "select {} from pg_catalog.generate_series(1, ({}::pg_catalog.text)::pg_catalog.int4) g",
        columns.join(", "),
        literal(REFUSAL_TEXT)
    );
    let view = ident(&table.name);
    let (read, write) = match lens {
        Lens::NoSecret => (roles.read, roles.write),
        Lens::Masked => (roles.masked_read, roles.masked_write),
    };
    format!(
        "create view {schema}.{view} as {body};\n\
         grant select on {schema}.{view} to {read}, {write};\n",
        schema = lens.schema(),
        read = ident(read),
        write = ident(write),
    )
}

/// The refusal views of both lenses ([`refusal_lens_sql`]).
pub(crate) fn refusal_view_sql(
    table: &CatalogTable,
    types: &HashMap<(u32, i16), String>,
    roles: &LensRoles<'_>,
) -> String {
    Lens::BOTH
        .iter()
        .map(|l| refusal_lens_sql(*l, table, types, roles))
        .collect()
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

/// The order in which the lens views are built: tables and materialized views first, then the
/// tenant views, every view after the views it reads (`facts` hold what each view reads, also
/// through other views). A view that reads another is built over the lens view of that one.
pub(crate) fn build_order<'a>(
    tables: &[&'a CatalogTable],
    facts: &[super::depends::RootFacts],
) -> Vec<&'a CatalogTable> {
    let rel_of = |t: &CatalogTable| t.columns.iter().find_map(|c| c.identity).map(|i| i.rel_oid);
    let (mut pending, mut ordered): (Vec<&CatalogTable>, Vec<&CatalogTable>) = tables
        .iter()
        .copied()
        .partition(|t| t.kind == RelationKind::View);
    let view_rels: HashSet<u32> = pending.iter().filter_map(|t| rel_of(t)).collect();
    let mut reads: HashMap<u32, HashSet<u32>> = HashMap::new();
    for f in facts
        .iter()
        .filter(|f| f.att == 0 && view_rels.contains(&f.rel))
    {
        let deps = reads.entry(f.rel).or_default();
        for r in f
            .whole_relations
            .iter()
            .chain(f.columns.iter().map(|(r, _)| r))
        {
            if *r != f.rel && view_rels.contains(r) {
                deps.insert(*r);
            }
        }
    }
    let mut built: HashSet<u32> = HashSet::new();
    while !pending.is_empty() {
        let (ready, rest): (Vec<_>, Vec<_>) = pending.into_iter().partition(|t| {
            rel_of(t)
                .and_then(|r| reads.get(&r))
                .is_none_or(|deps| deps.iter().all(|d| built.contains(d)))
        });
        if ready.is_empty() {
            // A cycle cannot exist; build what is left in the catalog's order.
            ordered.extend(rest);
            break;
        }
        for t in ready {
            built.extend(rel_of(t));
            ordered.push(t);
        }
        pending = rest;
    }
    ordered
}

/// Runs `SET LOCAL search_path TO path`: the setting lasts until the end of the caller's
/// transaction. (`set_config` is revoked from `mask`; the statement is not.) `path` is a list of
/// schema names written by this module, or the value that the session itself reported.
async fn set_search_path(conn: &mut PgConnection, path: &str) -> Result<(), EngineError> {
    run_sql(
        conn,
        &format!("set local search_path to {path}"),
        "set the search path",
    )
    .await
}

/// The text of `pg_get_viewdef` of every view of `public`, by name, with `public` first on the
/// search path so that the tenant's relations, functions and types come out unqualified. Run as
/// `mask`; it leaves the search path changed.
async fn view_definitions(conn: &mut PgConnection) -> Result<HashMap<String, String>, EngineError> {
    set_search_path(conn, "public, pg_catalog").await?;
    let rows = sqlx::query(
        "select c.relname::pg_catalog.text as name, pg_catalog.pg_get_viewdef(c.oid, false) as def
           from pg_catalog.pg_class c
          where c.relnamespace operator(pg_catalog.=) 'public'::pg_catalog.regnamespace
            and c.relkind operator(pg_catalog.=) 'v'::pg_catalog.\"char\"",
    )
    .persistent(false)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("read the definitions of the views", e))?;
    Ok(rows
        .into_iter()
        .map(|r| (r.get("name"), r.get("def")))
        .collect())
}

/// Whether an error of a statement that rebuilds a view means "this view cannot be rebuilt over
/// the lens" (an undefined function, operator, type or column, a type that does not fit, a
/// feature that Postgres does not support here) and not a failure of the connection.
fn cannot_rebuild(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d)
        if d.code().is_some_and(|c| c.starts_with("42") || c.starts_with("22") || c.starts_with("0A")))
}

/// What the rebuilt view of one lens reads, as `rebuild_view` checks it: whether a relation
/// outside the lens schema, or anything outside the allow-list, is in its stored tree.
const REBUILT_CHECK: &str = "select exists (
         select 1 from pg_catalog.regexp_matches(r.ev_action::pg_catalog.text, ':relid ([0-9]+)', 'g') as m(g)
          where not exists (select 1 from pg_catalog.pg_class k
                             where k.oid operator(pg_catalog.=) m.g[1]::pg_catalog.oid
                               and k.relnamespace operator(pg_catalog.=) c.relnamespace)) as foreign_relation,
       @UNLISTED@ as unlisted
  from pg_catalog.pg_class c
  join pg_catalog.pg_namespace n on n.oid operator(pg_catalog.=) c.relnamespace
  join pg_catalog.pg_rewrite r on r.ev_class operator(pg_catalog.=) c.oid
   and r.rulename operator(pg_catalog.=) '_RETURN'
 where n.nspname operator(pg_catalog.=) $2::pg_catalog.text
   and c.relname operator(pg_catalog.=) $3::pg_catalog.text";

/// The types of the columns of a view, as `(name, type OID, is a built-in base type)`.
const COLUMN_TYPES_OF_VIEW: &str = "select a.attname::pg_catalog.text as name,
       a.atttypid::pg_catalog.int8 as typ,
       (@BASE@) as base
  from pg_catalog.pg_class c
  join pg_catalog.pg_namespace n on n.oid operator(pg_catalog.=) c.relnamespace
  join pg_catalog.pg_attribute a on a.attrelid operator(pg_catalog.=) c.oid
  join pg_catalog.pg_type t on t.oid operator(pg_catalog.=) a.atttypid
 where n.nspname operator(pg_catalog.=) $1::pg_catalog.text
   and c.relname operator(pg_catalog.=) $2::pg_catalog.text
   and a.attnum operator(pg_catalog.>) 0 and not a.attisdropped";

/// Rebuilds the lens view of `lens` for the tenant view `table` over the lens relations
/// (decision 0004-39). Returns `false` (the caller then builds the refusal view of this lens)
/// when the view cannot be rebuilt: Postgres refuses the definition over the lens relations (a
/// function or operator that does not fit a pseudonym or the hidden marker, a type of the tenant
/// that the lens hides), or the rebuilt view reads a relation outside the lens schema or reaches
/// anything outside the allow-list.
///
/// 1. The definition of the view (`pg_get_viewdef`) is made again with
///    `search_path = <lens schema>, public, pg_catalog`: a name of the view's text finds the lens
///    relation of that name first (every relation of `public` has one), and a name that the lens
///    does not have (a type or function of the tenant) is found in `public`, exactly as in the
///    original.
/// 2. The stored tree of the new view is read: every relation in it must be in the lens schema
///    (this also catches a system catalog, a relation of another schema, and a relation that the
///    lens does not have), and the tree must be inside the allow-list.
/// 3. The view is made again as `select <lens columns> from (<definition>) q`: the lens masking
///    by class on top of a view that computes from masked values. The pseudonym function of a
///    column follows the type that the rebuilt column has now.
///
/// All of it runs under a savepoint, so a refusal leaves no trace.
async fn rebuild_view(
    conn: &mut PgConnection,
    lens: Lens,
    table: &CatalogTable,
    spec: &LensSpec,
    secret_reading: &HashSet<(u32, i16)>,
    definition: &str,
    roles: &LensRoles<'_>,
) -> Result<bool, EngineError> {
    let view = ident(&table.name);
    let schema = lens.schema();
    run_sql(conn, "savepoint ruum_rebuild", "open a savepoint").await?;
    set_search_path(conn, &format!("{schema}, public, pg_catalog")).await?;
    let rebuilt = rebuild_steps(conn, lens, table, spec, secret_reading, definition).await?;
    if !rebuilt {
        run_sql(conn, "rollback to savepoint ruum_rebuild", "undo a rebuild").await?;
    }
    run_sql(conn, "release savepoint ruum_rebuild", "close a savepoint").await?;
    if rebuilt {
        let grants = lens.grants(&view, roles);
        run_sql(conn, &grants, "grant the lens view").await?;
    }
    Ok(rebuilt)
}

async fn run_sql(conn: &mut PgConnection, sql: &str, what: &str) -> Result<(), EngineError> {
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(&mut *conn)
        .await
        .map_err(|e| internal(what, e))?;
    Ok(())
}

/// Steps 1 to 3 of [`rebuild_view`], after the savepoint. `Ok(false)`: not rebuildable.
async fn rebuild_steps(
    conn: &mut PgConnection,
    lens: Lens,
    table: &CatalogTable,
    spec: &LensSpec,
    secret_reading: &HashSet<(u32, i16)>,
    definition: &str,
) -> Result<bool, EngineError> {
    let schema = lens.schema();
    let view = ident(&table.name);
    // Step 1: the view over the lens relations, with all its columns as they come.
    let plain = format!(
        "create view {schema}.{view} as {}",
        definition.trim().trim_end_matches(';').trim()
    );
    match sqlx::query(sqlx::AssertSqlSafe(plain))
        .persistent(false)
        .execute(&mut *conn)
        .await
    {
        Ok(_) => {}
        Err(e) if cannot_rebuild(&e) => return Ok(false),
        Err(e) => return Err(internal("rebuild a view over the lens", e)),
    }
    // Step 2: it reads lens relations only, and nothing outside the allow-list.
    let check = REBUILT_CHECK.replace(
        "@UNLISTED@",
        &allow::unlisted("r.ev_action::pg_catalog.text"),
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "with recursive {} {check}",
        allow::ctes(1)
    )))
    .persistent(false)
    .bind(allow::row_functions())
    .bind(schema)
    .bind(&table.name)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| internal("check a rebuilt view", e))?;
    let Some(row) = row else { return Ok(false) };
    if row.get::<bool, _>("foreign_relation") || row.get::<bool, _>("unlisted") {
        return Ok(false);
    }
    // The types that the columns have over the lens.
    let types = sqlx::query(sqlx::AssertSqlSafe(
        COLUMN_TYPES_OF_VIEW.replace("@BASE@", BUILT_IN_BASE),
    ))
    .persistent(false)
    .bind(schema)
    .bind(&table.name)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| internal("read the types of a rebuilt view", e))?;
    let by_name: HashMap<String, u32> = types
        .into_iter()
        .filter(|r| r.get::<bool, _>("base"))
        .map(|r| (r.get::<String, _>("name"), r.get::<i64, _>("typ") as u32))
        .collect();
    let rebuilt_types: HashMap<(u32, i16), u32> = table
        .columns
        .iter()
        .filter_map(|c| {
            let i = c.identity?;
            Some(((i.rel_oid, i.attnum), *by_name.get(&c.name)?))
        })
        .collect();
    // Step 3: the lens columns on top of the definition.
    run_sql(
        conn,
        &format!("drop view {schema}.{view}"),
        "drop the plain rebuilt view",
    )
    .await?;
    let (nosecret, masked) = lens_columns(table, spec, &rebuilt_types, secret_reading);
    let columns = match lens {
        Lens::NoSecret => nosecret,
        Lens::Masked => masked,
    };
    let sql = rebuilt_view_sql(lens, table, &columns, definition);
    match sqlx::query(sqlx::AssertSqlSafe(sql))
        .persistent(false)
        .execute(&mut *conn)
        .await
    {
        Ok(_) => Ok(true),
        Err(e) if cannot_rebuild(&e) => Ok(false),
        Err(e) => Err(internal("rebuild a view over the lens", e)),
    }
}

/// Builds the lens views of every relation of `public` in `catalog`. Run as `mask` in the
/// caller's transaction (a savepoint is used), after [`drop_views`].
///
/// A table or a materialized view gets the lens views that read it. A tenant view gets lens views
/// that run its logic over the lens relations of the same schema ([`rebuild_view`], decision
/// 0004-39), built after the views it reads. A relation that cannot be shown
/// ([`code_relations`]), or a view that cannot be rebuilt in a lens, gets the refusal view in its
/// place (in that lens).
pub(crate) async fn build_views(
    conn: &mut PgConnection,
    catalog: &Catalog,
    spec: &LensSpec,
    roles: &LensRoles<'_>,
) -> Result<(), EngineError> {
    let bases = pseudonym_types(conn).await?;
    let facts = super::depends::root_facts(conn).await?;
    let secret_reading = secret_reading(catalog, spec, &facts);
    let edges = policy_edges(conn).await?;
    let indexes = index_relations(conn).await?;
    let refused = code_relations(catalog, spec, &facts, &edges, &indexes);
    // The column types of the relations that may get a refusal view: the refused ones, and every
    // view (a view that cannot be rebuilt gets one).
    let mut refusable = refused.clone();
    refusable.extend(
        catalog
            .tables
            .iter()
            .filter(|t| t.kind == RelationKind::View)
            .filter_map(|t| t.columns.iter().find_map(|c| c.identity).map(|i| i.rel_oid)),
    );
    let types = column_types(conn, &refusable).await?;
    let saved_path: String = sqlx::query_scalar("select pg_catalog.current_setting('search_path')")
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| internal("read the search path", e))?;
    let has_views = catalog
        .tables
        .iter()
        .any(|t| t.schema == "public" && t.kind == RelationKind::View);
    let definitions = if has_views {
        view_definitions(conn).await?
    } else {
        HashMap::new()
    };
    let public: Vec<&CatalogTable> = catalog
        .tables
        .iter()
        .filter(|t| t.schema == "public")
        .collect();
    for table in build_order(&public, &facts) {
        let refused_rel = table
            .columns
            .iter()
            .find_map(|c| c.identity)
            .is_some_and(|i| refused.contains(&i.rel_oid));
        if refused_rel {
            run_sql(
                conn,
                &refusal_view_sql(table, &types, roles),
                "build a refusal view",
            )
            .await?;
            continue;
        }
        if table.kind != RelationKind::View {
            run_sql(
                conn,
                &view_sql(table, spec, &bases, &secret_reading, roles),
                "build a lens view",
            )
            .await?;
            continue;
        }
        // A view whose definition is not known is refused, never read as it is.
        let Some(definition) = definitions.get(&table.name) else {
            run_sql(
                conn,
                &refusal_view_sql(table, &types, roles),
                "build a refusal view",
            )
            .await?;
            continue;
        };
        for lens in Lens::BOTH {
            let rebuilt =
                rebuild_view(conn, lens, table, spec, &secret_reading, definition, roles).await?;
            if !rebuilt {
                run_sql(
                    conn,
                    &refusal_lens_sql(lens, table, &types, roles),
                    "build a refusal view",
                )
                .await?;
            }
        }
    }
    set_search_path(conn, &saved_path).await?;
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
    use super::super::depends::RootFacts;
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
                        definition: None,
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
        let sql = view_sql(&table, &spec, &bases, &HashSet::new(), &roles);
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
                definition: None,
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
        let sql = view_sql(&table, &spec, &types, &HashSet::new(), &roles);
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

    /// Review-Q-1 N-9: a personal column of a tenant view that reads a secret column is masked
    /// like secret in both lenses; a column confirmed as normal keeps its class; a table is
    /// never treated so.
    #[test]
    fn a_view_over_a_secret_masks_its_personal_columns_like_secret() {
        let col = |name: &str, rel: u32, attnum: i16| CatalogColumn {
            name: name.to_string(),
            data_type: "text".into(),
            nullable: true,
            identity: Some(ColumnIdentity {
                rel_oid: rel,
                attnum,
                type_oid: oid::TEXT,
                definition: None,
            }),
        };
        let table = |name: &str, kind: RelationKind, columns: Vec<CatalogColumn>| CatalogTable {
            schema: "public".into(),
            name: name.into(),
            kind,
            columns,
            primary_key: vec![],
            foreign_keys: vec![],
        };
        let users = table(
            "users",
            RelationKind::Table,
            vec![col("id", 1, 1), col("token", 1, 2), col("name", 1, 3)],
        );
        let v = table(
            "v",
            RelationKind::View,
            vec![col("id", 2, 1), col("t", 2, 2), col("n", 2, 3)],
        );
        let names = table("names", RelationKind::View, vec![col("n", 3, 1)]);
        let catalog = Catalog {
            tables: vec![users, v.clone(), names],
        };
        let r = |t: &str, c: &str| ColumnRef {
            schema: "public".into(),
            table: t.into(),
            column: c.into(),
        };
        let spec = LensSpec {
            columns: vec![
                (r("users", "id"), DataClass::Normal),
                (r("users", "token"), DataClass::Secret),
                (r("users", "name"), DataClass::Personal),
                (r("v", "id"), DataClass::Normal),
            ],
        };
        // v reads users.id and users.token; names reads users.name only.
        let fact = |rel: u32, columns: Vec<(u32, i16)>| RootFacts {
            rel,
            att: 0,
            columns,
            whole_relations: vec![1],
            definer: false,
            whole_row: false,
            unlisted: false,
        };
        let facts = [
            fact(2, vec![(1, 1), (1, 2)]),
            fact(3, vec![(1, 3), (77, 1)]),
        ];
        let hits = secret_reading(&catalog, &spec, &facts);
        assert_eq!(hits, HashSet::from([(2, 0)]));
        let types = HashMap::from([
            ((2, 1), oid::TEXT),
            ((2, 2), oid::TEXT),
            ((2, 3), oid::TEXT),
        ]);
        let roles = LensRoles {
            read: "r_1",
            write: "r_2",
            masked_read: "r_3",
            masked_write: "r_4",
        };
        let sql = view_sql(&v, &spec, &types, &hits, &roles);
        for lens in ["ruum_nosecret", "ruum_masked"] {
            let line = sql
                .lines()
                .find(|l| l.starts_with(&format!("create view {lens}.")))
                .unwrap();
            assert!(line.contains("select \"id\", "), "{line}");
            assert!(
                line.contains(&format!("{} as \"t\"", hidden_expr("\"t\""))),
                "{line}"
            );
            assert!(
                line.contains(&format!("{} as \"n\"", hidden_expr("\"n\""))),
                "{line}"
            );
            assert!(!line.contains("ruum_mask.pseudonym"), "{line}");
        }
        // Without a secret dependency the same view pseudonymizes for the masked kinds.
        let sql = view_sql(&v, &spec, &types, &HashSet::new(), &roles);
        assert!(sql.contains("ruum_mask.pseudonym(\"t\") as \"t\""), "{sql}");
    }

    /// Review-Q-2-1 F-3 to F-5: a root reads a secret through a definer function reached through
    /// other functions, through a whole-row reference next to a relation with a secret column,
    /// or as a generated column over a secret column; a whole-row reference without such a
    /// relation does not count, and a generated column masks only itself.
    #[test]
    fn computed_columns_read_secrets_through_functions_whole_rows_and_generation() {
        let col = |name: &str, rel: u32, attnum: i16| CatalogColumn {
            name: name.to_string(),
            data_type: "text".into(),
            nullable: true,
            identity: Some(ColumnIdentity {
                rel_oid: rel,
                attnum,
                type_oid: oid::TEXT,
                definition: None,
            }),
        };
        let table = |name: &str, kind: RelationKind, columns: Vec<CatalogColumn>| CatalogTable {
            schema: "public".into(),
            name: name.into(),
            kind,
            columns,
            primary_key: vec![],
            foreign_keys: vec![],
        };
        let users = table(
            "users",
            RelationKind::Table,
            vec![col("id", 1, 1), col("token", 1, 2), col("tok2", 1, 3)],
        );
        let notes = table("notes", RelationKind::Table, vec![col("body", 5, 1)]);
        let catalog = Catalog {
            tables: vec![users.clone(), notes],
        };
        let r = |t: &str, c: &str| ColumnRef {
            schema: "public".into(),
            table: t.into(),
            column: c.into(),
        };
        let spec = LensSpec {
            columns: vec![
                (r("users", "id"), DataClass::Normal),
                (r("users", "token"), DataClass::Secret),
            ],
        };
        let base = |rel: u32, att: i16| RootFacts {
            rel,
            att,
            ..RootFacts::default()
        };
        let facts = [
            // A view that calls a definer function through a plain one.
            RootFacts {
                definer: true,
                whole_relations: vec![1],
                ..base(10, 0)
            },
            // A view with `to_json(u)` over users, which reads users.id too: Postgres keeps the
            // column entry only.
            RootFacts {
                whole_row: true,
                columns: vec![(1, 1)],
                ..base(11, 0)
            },
            // A view with `to_json(n)` over notes only.
            RootFacts {
                whole_row: true,
                whole_relations: vec![5],
                ..base(12, 0)
            },
            // A view that names users but reads id only, without a whole-row reference.
            RootFacts {
                columns: vec![(1, 1)],
                whole_relations: vec![1],
                ..base(13, 0)
            },
            // The generated column users.tok2 = upper(token).
            RootFacts {
                columns: vec![(1, 2), (1, 3)],
                whole_relations: vec![],
                ..base(1, 3)
            },
        ];
        let hits = secret_reading(&catalog, &spec, &facts);
        assert_eq!(hits, HashSet::from([(10, 0), (11, 0), (1, 3)]));
        // The generated column is masked like secret; the other unconfirmed column of the table
        // keeps its pseudonym, and the normal column stays.
        let types = HashMap::from([
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
        let sql = view_sql(&users, &spec, &types, &hits, &roles);
        for lens in ["ruum_nosecret", "ruum_masked"] {
            let line = sql
                .lines()
                .find(|l| l.starts_with(&format!("create view {lens}.")))
                .unwrap();
            assert!(line.contains("select \"id\", "), "{line}");
            assert!(
                line.contains(&format!("{} as \"tok2\"", hidden_expr("\"tok2\""))),
                "{line}"
            );
        }
    }

    fn table(name: &str, kind: RelationKind, rel: u32, columns: &[&str]) -> CatalogTable {
        CatalogTable {
            schema: "public".into(),
            name: name.into(),
            kind,
            columns: columns
                .iter()
                .enumerate()
                .map(|(i, n)| CatalogColumn {
                    name: n.to_string(),
                    data_type: "text".into(),
                    nullable: true,
                    identity: Some(ColumnIdentity {
                        rel_oid: rel,
                        attnum: i as i16 + 1,
                        type_oid: oid::TEXT,
                        definition: None,
                    }),
                })
                .collect(),
            primary_key: vec![],
            foreign_keys: vec![],
        }
    }

    /// Decision 0004-39: a view is built after the views it reads, whatever the catalog order.
    #[test]
    fn views_are_built_after_the_views_they_read() {
        let a = table("a", RelationKind::View, 2, &["x"]);
        let b = table("b", RelationKind::View, 4, &["x"]);
        let t = table("t", RelationKind::Table, 1, &["x"]);
        let z = table("z", RelationKind::View, 3, &["x"]);
        let reads = |rel: u32, reads: Vec<(u32, i16)>| RootFacts {
            rel,
            att: 0,
            columns: reads,
            ..RootFacts::default()
        };
        // a reads z, z reads t, b reads a (and, through a, z and t).
        let facts = [
            reads(2, vec![(3, 1), (1, 1)]),
            reads(3, vec![(1, 1)]),
            reads(4, vec![(2, 1), (3, 1), (1, 1)]),
        ];
        let order = build_order(&[&a, &b, &t, &z], &facts);
        let names: Vec<_> = order.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["t", "z", "a", "b"]);
    }

    /// Decision 0004-39: a policy cannot be shown through the lens when it reads a column that is
    /// not `normal`, a view, or a whole row of a table with such a column; a policy over normal
    /// columns, and a table that names a refused one, follow the rules.
    #[test]
    fn a_policy_that_reads_a_restricted_value_refuses_its_table() {
        let people = table("people", RelationKind::Table, 1, &["id", "pin"]);
        let tags = table("tags", RelationKind::Table, 2, &["id"]);
        let v = table("v", RelationKind::View, 3, &["x"]);
        let catalog = Catalog {
            tables: vec![people, tags, v],
        };
        let col = |t: &str, c: &str| ColumnRef {
            schema: "public".into(),
            table: t.into(),
            column: c.into(),
        };
        let spec = LensSpec {
            columns: vec![
                (col("people", "id"), DataClass::Normal),
                (col("people", "pin"), DataClass::Secret),
                (col("tags", "id"), DataClass::Normal),
            ],
        };
        let code = |edges: Vec<(u32, PolicyFact)>| {
            let mut c: Vec<_> = code_relations(&catalog, &spec, &[], &edges, &[])
                .into_iter()
                .collect();
            c.sort();
            c
        };
        let reads = |rel: u32, column: i16| PolicyFact::Reads { rel, column };
        assert_eq!(
            code(vec![(2, reads(2, 1)), (2, reads(2, 0))]),
            Vec::<u32>::new()
        );
        assert_eq!(
            code(vec![(1, reads(1, 1))]),
            Vec::<u32>::new(),
            "pin is attnum 2"
        );
        assert_eq!(code(vec![(1, reads(1, 2))]), [1], "a secret column");
        assert_eq!(
            code(vec![(2, reads(1, 2))]),
            [2],
            "a restricted column of another table"
        );
        assert_eq!(code(vec![(2, reads(3, 0))]), [2], "a view");
        assert_eq!(code(vec![(2, PolicyFact::WholeRow)]), Vec::<u32>::new());
        assert_eq!(code(vec![(1, PolicyFact::WholeRow)]), [1]);
        assert_eq!(
            code(vec![(2, PolicyFact::WholeRow), (2, reads(1, 0))]),
            [2],
            "a whole row next to a table with a restricted column"
        );
        assert_eq!(code(vec![(2, PolicyFact::Outside)]), [2]);
        assert_eq!(
            code(vec![(1, PolicyFact::Outside), (2, reads(1, 0))]),
            [1, 2],
            "a table whose policy names a refused table"
        );
    }

    /// The rebuilt lens view is the lens columns over the definition of the view, in the lens
    /// schema of its kind; a refusal view is made per lens.
    #[test]
    fn a_rebuilt_view_puts_the_lens_columns_over_the_definition() {
        let v = table("v", RelationKind::View, 3, &["a", "b"]);
        let spec = LensSpec {
            columns: vec![(
                ColumnRef {
                    schema: "public".into(),
                    table: "v".into(),
                    column: "a".into(),
                },
                DataClass::Normal,
            )],
        };
        let types = HashMap::from([((3, 1), oid::TEXT), ((3, 2), oid::TEXT)]);
        let (nosecret, masked) = lens_columns(&v, &spec, &types, &HashSet::new());
        let def = " SELECT t.a,\n    t.b\n   FROM t;\n";
        let sql = rebuilt_view_sql(Lens::Masked, &v, &masked, def);
        assert_eq!(
            sql,
            "create view ruum_masked.\"v\" as select \"a\", ruum_mask.pseudonym(\"b\") as \"b\" \
             from (SELECT t.a,\n    t.b\n   FROM t) as \"q\""
        );
        let sql = rebuilt_view_sql(Lens::NoSecret, &v, &nosecret, def);
        assert!(
            sql.starts_with("create view ruum_nosecret.\"v\" as select \"a\", \"b\" from ("),
            "{sql}"
        );
        let roles = LensRoles {
            read: "r_1",
            write: "r_2",
            masked_read: "r_3",
            masked_write: "r_4",
        };
        let refusal = refusal_lens_sql(Lens::Masked, &v, &HashMap::new(), &roles);
        assert!(
            refusal.starts_with("create view ruum_masked.\"v\" as select null::pg_catalog.text"),
            "{refusal}"
        );
        assert!(refusal.contains("to \"r_3\", \"r_4\""), "{refusal}");
        assert!(!refusal.contains("ruum_nosecret"), "{refusal}");
        assert_eq!(
            refusal_view_sql(&v, &HashMap::new(), &roles)
                .matches("create view")
                .count(),
            2
        );
    }
}
