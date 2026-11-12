//! The function allow-list (decision 0004-38 (b) and (d), design 8.5; review-Q-2-s21-2 R-2,
//! review-Q-2-harden H-1).
//!
//! A function with an effect that outlives the expression that calls it (a session setting, an
//! advisory lock, a random seed, a sequence, a large object) is a channel: code that runs on the
//! real rows can write a secret into it, and a masked statement can read it back. A list of such
//! functions is never complete (review-Q-2-harden H-1: `setseed`), so the engine works the other
//! way round. Two rules, both fail closed:
//!
//! 1. **Code that runs on the real rows** (a view of the tenant, a row-level security policy, a
//!    `CHECK` constraint, an index expression, a generated column) may call only **allowed
//!    functions**: a built-in function that Postgres does not mark `VOLATILE` and that is named
//!    in [`ROW_FUNCTIONS`], the function of a built-in operator, or the function of a built-in
//!    cast. Anything else (a function of the tenant or of an extension, a session setting, a
//!    lock, a seed, a sequence, a large object, a function that runs a query given as text) is
//!    outside the list.
//!    - A relation whose view or policy reaches such a function is not part of the masked lens:
//!      the lens builds a *refusal view* in its place (`lens::refusal_view_sql`), and a statement
//!      that reads it gets `statement_not_allowed` (`tenant_code_on_live`).
//!    - Such an object counts as tenant code for writes (`depends::TENANT_CODE_SQL`, kind
//!      `unlisted_functions`).
//!    - Built-in functions are *pinned* in Postgres: `pg_depend` records no dependency on them.
//!      The engine finds them in the stored node tree of the object (`:funcid 1234`, `:opfuncid`,
//!      `:aggfnoid`, `:winfnoid`, `:opno`, ...), and looks each OID up in the allowed set.
//!    - **Domain checks** (review-Q-2-harden-2 F-1). A cast to a domain runs the checks of the
//!      domain on the value, also inside a view or a policy that is otherwise on the list. The
//!      engine follows every type that a stored tree names as the result of a coercion
//!      (`:resulttype`, ...) to the domains it reaches: the base type of a domain, the element
//!      of an array, the subtype of a range, the fields of a composite type, and the types that
//!      the check of a domain itself coerces to. A domain with a `CHECK` that is outside the list
//!      (a function of the tenant or of an extension, a built-in function not on the list, an
//!      operator of the tenant) makes every type that reaches it *unvetted* (`bad_types`), and a
//!      tree that coerces to an unvetted type is outside the list. A domain whose checks are on
//!      the list is fine.
//!    - **Index support** (review-Q-2-harden-2 F-2). A scan of an index calls the support
//!      functions of its operator classes, and the operators of the class, on the raw keys. The
//!      slots `pg_amproc.amproc` and `pg_amop.amopopr` can hold a function of an extension, or of
//!      the tenant when a superuser made the operator class. A relation with an index whose
//!      operator class holds a function outside the list ([`support_cte`]) is treated as code
//!      outside the list. The built-in operator families are on the list: their support
//!      functions are non-volatile functions of `pg_catalog`.
//!    - **Type functions** (review-Q-2-harden-3 F-3). A cast to a type calls its input function,
//!      and a value of the type calls its output, modifier and (for a range) canonical and
//!      subtype-difference functions. A type with such a function that is not built in (OID
//!      16384 or above: a function of the tenant or of an extension) is unvetted (`bad_types`),
//!      like a domain with a check outside the list.
//!    - The rebuild of tenant views over the lens (decision 0004-39, `lens::rebuild_view`) is the
//!      structural cure: a tenant view only ever runs on masked values. This list stays as
//!      defence in depth.
//! 2. **The statement itself.** The cluster set-up revokes `EXECUTE` on every `VOLATILE` function
//!    of `pg_catalog` from `PUBLIC`, except the few in [`STATEMENT_VOLATILE`] (they carry no
//!    state that a secret can reach). Postgres marks a function `IMMUTABLE` or `STABLE` only when
//!    it cannot write, so the rest cannot be a channel. The readers of a session setting
//!    ([`SETTING_READERS`]: `current_setting`) are revoked too, and given back only to the kinds
//!    that see every value: code that wrote a setting cannot hand it to a reader that way. Postgres checks `EXECUTE` against the
//!    role of the session also for a function that a view calls, and for a function that runs a
//!    query given as text, so the line holds on every path. The `owner` kind gets the revoked
//!    functions back ([`crate::postgres::setup::grants_sql`]); the engine refuses a database
//!    where another kind may call one ([`crate::postgres::setup::check_volatile_revoked`]).

/// The built-in functions that may run on the real rows (rule 1 of the module docs), by name. A
/// function that Postgres marks `VOLATILE` is never allowed, whatever its name. The functions of
/// built-in operators and casts are allowed without a name. Plain functions: comparison,
/// math, strings, dates and times, arrays, JSON, text search; aggregates and window functions.
pub(crate) const ROW_FUNCTIONS: &[&str] = &[
    // Aggregates.
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "array_agg",
    "string_agg",
    "bool_and",
    "bool_or",
    "every",
    "bit_and",
    "bit_or",
    "bit_xor",
    "any_value",
    "json_agg",
    "jsonb_agg",
    "json_object_agg",
    "jsonb_object_agg",
    "stddev",
    "stddev_pop",
    "stddev_samp",
    "variance",
    "var_pop",
    "var_samp",
    "corr",
    "covar_pop",
    "covar_samp",
    "regr_avgx",
    "regr_avgy",
    "regr_count",
    "regr_intercept",
    "regr_r2",
    "regr_slope",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
    "mode",
    "percentile_cont",
    "percentile_disc",
    // Window functions.
    "row_number",
    "rank",
    "dense_rank",
    "percent_rank",
    "cume_dist",
    "ntile",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "nth_value",
    // Comparison and null handling.
    "num_nulls",
    "num_nonnulls",
    "pg_typeof",
    // Math.
    "abs",
    "ceil",
    "ceiling",
    "floor",
    "round",
    "trunc",
    "sign",
    "sqrt",
    "cbrt",
    "power",
    "pow",
    "exp",
    "ln",
    "log",
    "log10",
    "mod",
    "div",
    "gcd",
    "lcm",
    "degrees",
    "radians",
    "pi",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "sinh",
    "cosh",
    "tanh",
    "width_bucket",
    "factorial",
    "scale",
    "min_scale",
    "trim_scale",
    "erf",
    "erfc",
    "generate_series",
    "bit_count",
    "get_bit",
    "get_byte",
    // Strings.
    "lower",
    "upper",
    "initcap",
    "length",
    "char_length",
    "character_length",
    "octet_length",
    "bit_length",
    "substr",
    "substring",
    "left",
    "right",
    "lpad",
    "rpad",
    "ltrim",
    "rtrim",
    "btrim",
    "trim",
    "replace",
    "translate",
    "repeat",
    "reverse",
    "concat",
    "concat_ws",
    "position",
    "strpos",
    "starts_with",
    "split_part",
    "string_to_array",
    "string_to_table",
    "array_to_string",
    "regexp_replace",
    "regexp_match",
    "regexp_matches",
    "regexp_split_to_array",
    "regexp_split_to_table",
    "regexp_count",
    "regexp_instr",
    "regexp_substr",
    "md5",
    "sha224",
    "sha256",
    "sha384",
    "sha512",
    "ascii",
    "chr",
    "format",
    "quote_ident",
    "quote_literal",
    "quote_nullable",
    "to_hex",
    "encode",
    "decode",
    "convert_to",
    "convert_from",
    "overlay",
    "normalize",
    "is_normalized",
    "similar_to_escape",
    "unistr",
    "to_ascii",
    // Dates and times.
    "now",
    "statement_timestamp",
    "transaction_timestamp",
    "date_part",
    "date_trunc",
    "extract",
    "age",
    "make_date",
    "make_time",
    "make_timestamp",
    "make_timestamptz",
    "make_interval",
    "to_char",
    "to_date",
    "to_timestamp",
    "to_number",
    "justify_days",
    "justify_hours",
    "justify_interval",
    "isfinite",
    "timezone",
    "date_bin",
    "date_add",
    "date_subtract",
    // Arrays and ranges.
    "array_length",
    "array_dims",
    "array_ndims",
    "array_upper",
    "array_lower",
    "cardinality",
    "array_position",
    "array_positions",
    "array_append",
    "array_prepend",
    "array_cat",
    "array_remove",
    "array_replace",
    "array_fill",
    "unnest",
    "generate_subscripts",
    "isempty",
    "lower_inc",
    "upper_inc",
    "lower_inf",
    "upper_inf",
    "range_merge",
    // JSON.
    "to_json",
    "to_jsonb",
    "row_to_json",
    "array_to_json",
    "json_build_object",
    "jsonb_build_object",
    "json_build_array",
    "jsonb_build_array",
    "json_object",
    "jsonb_object",
    "json_extract_path",
    "json_extract_path_text",
    "jsonb_extract_path",
    "jsonb_extract_path_text",
    "json_typeof",
    "jsonb_typeof",
    "json_array_length",
    "jsonb_array_length",
    "json_array_elements",
    "jsonb_array_elements",
    "json_array_elements_text",
    "jsonb_array_elements_text",
    "json_each",
    "jsonb_each",
    "json_each_text",
    "jsonb_each_text",
    "json_object_keys",
    "jsonb_object_keys",
    "jsonb_set",
    "jsonb_insert",
    "jsonb_pretty",
    "json_strip_nulls",
    "jsonb_strip_nulls",
    "jsonb_path_exists",
    "jsonb_path_match",
    "jsonb_path_query",
    "jsonb_path_query_array",
    "jsonb_path_query_first",
    // Text search.
    "to_tsvector",
    "to_tsquery",
    "plainto_tsquery",
    "phraseto_tsquery",
    "websearch_to_tsquery",
    "ts_rank",
    "ts_rank_cd",
    "ts_headline",
    "setweight",
    "strip",
    "tsvector_to_array",
    // Network addresses.
    "host",
    "family",
    "masklen",
    "netmask",
    "network",
    "abbrev",
];

/// The `VOLATILE` built-in functions that a statement of a masked or read-only session may still
/// call (rule 2 of the module docs). None of them can carry a value from one expression to
/// another: a clock, an unseeded random number, a sequence number (`nextval` fills the defaults
/// of an insert), a sleep, and the two calls that end a session of the caller's own role (the
/// engine ends the sessions of a database from a session of each role when it drains a copy).
/// They are not in [`ROW_FUNCTIONS`], so code that runs on the real rows
/// may not call them. The cluster set-up (`deploy/dev/postgres-data/initdb/10-ruum-data.sh`)
/// carries the same list; keep both the same.
pub(crate) const STATEMENT_VOLATILE: &[&str] = &[
    "nextval",
    "currval",
    "lastval",
    "gen_random_uuid",
    "uuidv7",
    "clock_timestamp",
    "timeofday",
    "random",
    "pg_sleep",
    "pg_sleep_for",
    "pg_sleep_until",
    "pg_cancel_backend",
    "pg_terminate_backend",
];

/// Built-in functions that are not `VOLATILE` but read a session setting: a custom setting
/// (`ruum.x`) that code wrote is a value a reader must not get back (review-Q-2-s21-2 R-1 and
/// suggestion 4; STATUS "Next cards" 0). The cluster set-up revokes them from `PUBLIC` with the
/// volatile functions; the engine gives them back to the kinds that see every value (`owner`,
/// `secret_read`) and to `mask`, never to `read`, `write`, `masked_read` or `masked_write`.
/// The cluster set-up carries the same list; keep both the same.
pub(crate) const SETTING_READERS: &[&str] = &["current_setting"];

/// [`ROW_FUNCTIONS`] as the owned list that the queries bind.
pub(crate) fn row_functions() -> Vec<String> {
    ROW_FUNCTIONS.iter().map(|s| (*s).to_string()).collect()
}

/// The SQL condition, on `pg_catalog.pg_proc p`, of the functions that the cluster set-up revokes
/// from `PUBLIC` (rule 2): `VOLATILE` functions of `pg_catalog` that are not in
/// [`STATEMENT_VOLATILE`], and the setting readers ([`SETTING_READERS`]). Aggregates, window
/// functions and procedures are left out: none of them is `VOLATILE` and callable without a
/// statement kind that the rules refuse.
pub(crate) fn revoked_condition() -> String {
    let ok = STATEMENT_VOLATILE.join(",");
    let readers = SETTING_READERS.join(",");
    format!(
        "p.pronamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace \
         and p.prokind operator(pg_catalog.=) 'f'::pg_catalog.\"char\" \
         and ((p.provolatile operator(pg_catalog.=) 'v'::pg_catalog.\"char\" \
               and p.proname::pg_catalog.text operator(pg_catalog.<>) all ('{{{ok}}}'::pg_catalog.text[])) \
              or p.proname::pg_catalog.text operator(pg_catalog.=) any ('{{{readers}}}'::pg_catalog.text[]))"
    )
}

/// The SQL condition, on `pg_catalog.pg_proc p`, of the setting readers ([`SETTING_READERS`]).
pub(crate) fn setting_readers_condition() -> String {
    let readers = SETTING_READERS.join(",");
    format!(
        "p.pronamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace \
         and p.prokind operator(pg_catalog.=) 'f'::pg_catalog.\"char\" \
         and p.proname::pg_catalog.text operator(pg_catalog.=) any ('{{{readers}}}'::pg_catalog.text[])"
    )
}

/// The common table expression `allowed(oid)`: the allowed functions (rule 1), with the names
/// of [`ROW_FUNCTIONS`] in the bind parameter `$param` (`text[]`). Every name and operator is
/// qualified: this runs in the session of an untrusted request too.
pub(crate) fn allowed_cte(param: usize) -> String {
    format!(
        "allowed as (
  select p.oid from pg_catalog.pg_proc p
   where p.pronamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace
     and p.provolatile operator(pg_catalog.<>) 'v'::pg_catalog.\"char\"
     and (p.proname::pg_catalog.text operator(pg_catalog.=) any (${param}::pg_catalog.text[])
          or p.oid operator(pg_catalog.=) any (array(
               select o.oprcode::pg_catalog.oid from pg_catalog.pg_operator o
                where o.oprnamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace))
          or p.oid operator(pg_catalog.=) any (array(
               select c.castfunc from pg_catalog.pg_cast c
                where c.castfunc operator(pg_catalog.<>) 0::pg_catalog.oid))))"
    )
}

/// Every common table expression that the conditions of this module need, for the bind parameter
/// `$param` (the names of [`ROW_FUNCTIONS`]): [`allowed_cte`], [`bad_types_cte`] and
/// [`support_cte`]. The `WITH` list that holds them must be `WITH RECURSIVE`.
pub(crate) fn ctes(param: usize) -> String {
    format!(
        "{},\n{},\n{}",
        allowed_cte(param),
        bad_types_cte(),
        support_cte()
    )
}

/// The OID fields of a stored node tree that name a function.
const FUNCTION_FIELDS: &str = "funcid|opfuncid|aggfnoid|winfnoid|tsmhandler|hashfuncid|negfuncid|\
     startInRangeFunc|endInRangeFunc";
/// The OID fields that name an operator (the operator's function is looked up).
const OPERATOR_FIELDS: &str = "opno|eqop|sortop|hashop";
/// The OID fields that name the type of a value that the tree coerces, builds or returns. A
/// coercion to a domain (`CoerceToDomain`) runs the checks of the domain; the input function of a
/// domain runs them too (`CoerceViaIO` to an array or composite of the domain). A plain
/// reference to a column (`:vartype`) runs nothing and is not listed.
const TYPE_FIELDS: &str = "resulttype|array_typeid|element_typeid|row_typeid|paramtype|aggtype|\
     wintype|funcresulttype";

/// The common table expressions for the types that a tree coerces to (review-Q-2-harden-2 F-1),
/// after [`allowed_cte`] in the same `WITH RECURSIVE` list:
///
/// - `type_edges(src, dst)`: which type holds which (a domain its base type and the types that
///   its own checks coerce to, an array its element, a range its subtype, a multirange its range,
///   a composite type the types of its fields);
/// - `bad_types(oid)`: the domains that have a `CHECK` outside the allow-list, the types with an
///   input, output or other type function (or a range with a canonical or subtype-difference
///   function) that is not a built-in function, and every type that reaches one through
///   `type_edges`.
pub(crate) fn bad_types_cte() -> String {
    let check_outside = unlisted_calls("k.conbin::pg_catalog.text");
    format!(
        "type_edges(src, dst) as (
  select t.oid, t.typbasetype from pg_catalog.pg_type t
   where t.typtype operator(pg_catalog.=) 'd'::pg_catalog.\"char\"
  union all
  select t.oid, t.typelem from pg_catalog.pg_type t where t.typelem operator(pg_catalog.<>) 0::pg_catalog.oid
  union all
  select r.rngtypid, r.rngsubtype from pg_catalog.pg_range r
  union all
  select r.rngmultitypid, r.rngtypid from pg_catalog.pg_range r
  union all
  select t.oid, a.atttypid
    from pg_catalog.pg_type t
    join pg_catalog.pg_attribute a on a.attrelid operator(pg_catalog.=) t.typrelid
   where t.typtype operator(pg_catalog.=) 'c'::pg_catalog.\"char\"
     and t.typrelid operator(pg_catalog.<>) 0::pg_catalog.oid
     and a.attnum operator(pg_catalog.>) 0::pg_catalog.int2 and not a.attisdropped
  union all
  select k.contypid, m.g[1]::pg_catalog.oid
    from pg_catalog.pg_constraint k,
         pg_catalog.regexp_matches(k.conbin::pg_catalog.text, ':(?:{TYPE_FIELDS}) ([1-9][0-9]*)', 'g') as m(g)
   where k.contypid operator(pg_catalog.<>) 0::pg_catalog.oid
     and k.contype operator(pg_catalog.=) 'c'::pg_catalog.\"char\"
),
bad_types(oid) as (
  select k.contypid from pg_catalog.pg_constraint k
   where k.contypid operator(pg_catalog.<>) 0::pg_catalog.oid
     and k.contype operator(pg_catalog.=) 'c'::pg_catalog.\"char\"
     and {check_outside}
  union
  -- A type whose own input, output, receive, send, modifier, analyze or subscript function is
  -- not a built-in function (OID 16384 or above: a function of the tenant or of an extension):
  -- a cast to it runs that function (review-Q-2-harden-3 F-3).
  select t.oid from pg_catalog.pg_type t
   where t.typinput::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typoutput::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typreceive::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typsend::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typmodin::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typmodout::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typanalyze::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or t.typsubscript::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
  union
  -- A range type whose canonical or subtype-difference function is not a built-in function (the
  -- range constructor and the range index code call them on the bounds).
  select r.rngtypid from pg_catalog.pg_range r
   where r.rngcanonical::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
      or r.rngsubdiff::pg_catalog.oid operator(pg_catalog.>=) 16384::pg_catalog.oid
  union
  select e.src from type_edges e join bad_types b on b.oid operator(pg_catalog.=) e.dst
)"
    )
}

/// The common table expression `support(oid)` (after [`allowed_cte`]): the functions that may
/// run as the support function of an operator class on the raw keys of an index (review
/// Q-2-harden-2 F-2): the allowed functions, and the non-volatile functions of `pg_catalog` that
/// a built-in operator family names as a support function (`btint4cmp`, `hashtext`, the GiST and
/// GIN functions of the built-in classes).
pub(crate) fn support_cte() -> String {
    "support as (
  select a.oid from allowed a
  union
  select ap.amproc::pg_catalog.oid
    from pg_catalog.pg_amproc ap
    join pg_catalog.pg_proc p on p.oid operator(pg_catalog.=) ap.amproc::pg_catalog.oid
   where ap.amprocfamily operator(pg_catalog.<) 16384::pg_catalog.oid
     and p.pronamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace
     and p.provolatile operator(pg_catalog.<>) 'v'::pg_catalog.\"char\"
)"
    .to_string()
}

/// A SQL condition that is true when the index `index` (the alias of a `pg_catalog.pg_index`
/// row) has an operator class whose family holds a support function (`pg_amproc`) or an operator
/// (`pg_amop`) outside the allow-list. It needs the CTEs [`allowed_cte`] and [`support_cte`] in
/// scope. The family is checked as a whole, whichever of its types the index uses: this is
/// conservative.
pub(crate) fn unlisted_index_support(index: &str) -> String {
    format!(
        "exists (
  select 1
    from pg_catalog.unnest({index}.indclass::pg_catalog.oid[]) as k(opc)
    join pg_catalog.pg_opclass oc on oc.oid operator(pg_catalog.=) k.opc
   where exists (select 1 from pg_catalog.pg_amproc ap
                  where ap.amprocfamily operator(pg_catalog.=) oc.opcfamily
                    and not exists (select 1 from support s
                                     where s.oid operator(pg_catalog.=) ap.amproc::pg_catalog.oid))
      or exists (select 1 from pg_catalog.pg_amop ao
                   join pg_catalog.pg_operator o on o.oid operator(pg_catalog.=) ao.amopopr
                  where ao.amopfamily operator(pg_catalog.=) oc.opcfamily
                    and not exists (select 1 from allowed a
                                     where a.oid operator(pg_catalog.=) o.oprcode::pg_catalog.oid)))"
    )
}

/// A SQL condition that is true when the stored node tree `tree` (a text expression: a
/// `pg_node_tree` cast to `text`) names a function or operator outside the allowed set. It needs
/// the CTE [`allowed_cte`] in scope. A tree that is `NULL` or names nothing gives false.
pub(crate) fn unlisted_calls(tree: &str) -> String {
    format!(
        "(exists (select 1 from pg_catalog.regexp_matches({tree}, ':(?:{FUNCTION_FIELDS}) ([1-9][0-9]*)', 'g') as m(g)
                  where not exists (select 1 from allowed a
                                     where a.oid operator(pg_catalog.=) m.g[1]::pg_catalog.oid))
          or exists (select 1 from pg_catalog.regexp_matches({tree}, ':(?:{OPERATOR_FIELDS}) ([1-9][0-9]*)', 'g') as m(g)
                      where not exists (select 1 from pg_catalog.pg_operator o
                                          join allowed a on a.oid operator(pg_catalog.=) o.oprcode::pg_catalog.oid
                                         where o.oid operator(pg_catalog.=) m.g[1]::pg_catalog.oid))
          or exists (select 1 from pg_catalog.regexp_matches({tree}, ':opnos \\(([0-9 ]+)\\)', 'g') as m(g),
                          pg_catalog.regexp_split_to_table(m.g[1], ' ') as s(op)
                      where s.op operator(pg_catalog.<>) ''
                        and not exists (select 1 from pg_catalog.pg_operator o
                                          join allowed a on a.oid operator(pg_catalog.=) o.oprcode::pg_catalog.oid
                                         where o.oid operator(pg_catalog.=) s.op::pg_catalog.oid)))"
    )
}

/// A SQL condition that is true when the stored node tree `tree` coerces a value to a type that
/// reaches a domain with a check outside the allow-list (review-Q-2-harden-2 F-1). It needs the
/// CTEs [`allowed_cte`] and [`bad_types_cte`] in scope.
pub(crate) fn unlisted_domains(tree: &str) -> String {
    format!(
        "exists (select 1 from pg_catalog.regexp_matches({tree}, ':(?:{TYPE_FIELDS}) ([1-9][0-9]*)', 'g') as m(g)
                  where m.g[1]::pg_catalog.oid operator(pg_catalog.=) any (array(select b.oid from bad_types b)))"
    )
}

/// A SQL condition that is true when the stored node tree `tree` is outside the allow-list: it
/// names a function or operator outside the allowed set ([`unlisted_calls`]), or it coerces a
/// value to a type that reaches a domain with a check outside the allowed set
/// ([`unlisted_domains`]). It needs the CTEs [`allowed_cte`] and [`bad_types_cte`] in scope.
pub(crate) fn unlisted(tree: &str) -> String {
    format!("({} or {})", unlisted_calls(tree), unlisted_domains(tree))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lists_hold_no_channel() {
        // The functions that carry state must never be named in the list of rule 1.
        for bad in [
            "set_config",
            "current_setting",
            "setseed",
            "random",
            "nextval",
            "setval",
            "currval",
            "lastval",
            "pg_sleep",
            "pg_notify",
            "query_to_xml",
            "ts_stat",
            "lo_get",
            "lo_from_bytea",
            "pg_advisory_lock",
            "pg_advisory_xact_lock",
            "pg_try_advisory_xact_lock",
            "pg_lock_status",
            "clock_timestamp",
            "gen_random_uuid",
            "dblink",
        ] {
            assert!(!ROW_FUNCTIONS.contains(&bad), "{bad}");
        }
        // The names of the two lists are different: a volatile name is never also a row name.
        for name in STATEMENT_VOLATILE {
            assert!(!ROW_FUNCTIONS.contains(name), "{name}");
        }
        // No name twice.
        let mut seen = std::collections::HashSet::new();
        for name in ROW_FUNCTIONS {
            assert!(seen.insert(*name), "{name} twice");
        }
    }
}
