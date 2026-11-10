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
//! 2. **The statement itself.** The cluster set-up revokes `EXECUTE` on every `VOLATILE` function
//!    of `pg_catalog` from `PUBLIC`, except the few in [`STATEMENT_VOLATILE`] (they carry no
//!    state that a secret can reach). Postgres marks a function `IMMUTABLE` or `STABLE` only when
//!    it cannot write, so the rest cannot be a channel. Postgres checks `EXECUTE` against the
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

/// [`ROW_FUNCTIONS`] as the owned list that the queries bind.
pub(crate) fn row_functions() -> Vec<String> {
    ROW_FUNCTIONS.iter().map(|s| (*s).to_string()).collect()
}

/// The SQL condition, on `pg_catalog.pg_proc p`, of the functions that the cluster set-up revokes
/// from `PUBLIC` (rule 2): `VOLATILE` functions of `pg_catalog` that are not in
/// [`STATEMENT_VOLATILE`]. Aggregates, window functions and procedures are left out: none of
/// them is `VOLATILE` and callable without a statement kind that the rules refuse.
pub(crate) fn revoked_condition() -> String {
    let ok = STATEMENT_VOLATILE.join(",");
    format!(
        "p.pronamespace operator(pg_catalog.=) 'pg_catalog'::pg_catalog.regnamespace \
         and p.prokind operator(pg_catalog.=) 'f'::pg_catalog.\"char\" \
         and p.provolatile operator(pg_catalog.=) 'v'::pg_catalog.\"char\" \
         and p.proname::pg_catalog.text operator(pg_catalog.<>) all ('{{{ok}}}'::pg_catalog.text[])"
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

/// The OID fields of a stored node tree that name a function.
const FUNCTION_FIELDS: &str = "funcid|opfuncid|aggfnoid|winfnoid|tsmhandler|hashfuncid|negfuncid|\
     startInRangeFunc|endInRangeFunc";
/// The OID fields that name an operator (the operator's function is looked up).
const OPERATOR_FIELDS: &str = "opno|eqop|sortop|hashop";

/// A SQL condition that is true when the stored node tree `tree` (a text expression: a
/// `pg_node_tree` cast to `text`) names a function or operator outside the allowed set. It needs
/// the CTE [`allowed_cte`] in scope. A tree that is `NULL` or names nothing gives false.
pub(crate) fn unlisted(tree: &str) -> String {
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
