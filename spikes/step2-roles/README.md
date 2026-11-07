# Spike S-1: the Postgres engine security model

Card S-1 of `docs/STEP2.md`. The spike proves, on a real PostgreSQL 17, the role kinds, the lens
views and the pooled-session rules of "The engine security model" before the kinds of
`database_roles` freeze. The result and the spec changes are in
`docs/decisions/0004-step2-spec.md`, item 34.

## Run it

On the build server (`scripts/buildbox/README.md`):

```sh
scripts/buildbox/run.sh <name> <worktree> -- spikes/step2-roles/spike.sh
```

The script uses the agent Postgres of `run.sh` (`pg-$BUILDBOX_NAME`, the stack's Postgres 17
image) or the container in `S1_PG_CONTAINER`. It changes that cluster: it creates the role
`ruum_dbhost` and databases and roles named `d_...` and `r_...`, and it revokes `CONNECT` on
`postgres` and `template1` from `PUBLIC`. Do not point it at a shared Postgres. It drops what an
earlier run made, so it can run again.

The superuser is used only for the cluster set-up of card I-1 and to look at results. All
platform DDL runs as `ruum_dbhost` (no superuser). Every untrusted statement runs in a session
that logs in as the per-database role, with a short-lived password, over TCP.

## Files

| File | What |
|---|---|
| `spike.sh` | The checks. It prints `PASS` or `FAIL` per check and exits non-zero on a failure. |
| `values.tsv` | The values for the pseudonym comparison (point 6). |
| `pseudonym/` | The Rust side of the pseudonym: `canonical_text` and HMAC-SHA-256 (its own small cargo workspace, not a member of the Ruum workspace). |

## What it checks

1. Each LOGIN kind (`owner`, `read`, `write`, `masked_read`, `masked_write`, `secret_read`) is a
   member of no role and cannot reach another role, `ruum_dbhost`, `mask` or the superuser with
   `RESET ROLE`, `SET ROLE`, `set_config('role', ...)`, `SET SESSION AUTHORIZATION`, a `DO`
   block or a function. It cannot connect to another database, `postgres` or `template1`, and
   cannot make temporary tables.
2. The masked kinds read `personal` columns as pseudonyms in every clause (`WHERE`, `JOIN`,
   `ORDER BY`, `GROUP BY`, `HAVING`, aggregates, CTEs, `EXPLAIN`) and cannot read the base
   tables, `ruum_mask.key`, `pg_stats` values or the key through function bodies or settings.
3. `ruum_mask.pseudonym` runs from a lens view, but no session can call it by name.
4. `masked_write` writes plain columns through the view, cannot write masked columns, and a
   `WHERE` on a masked column compares the pseudonym. Constraint errors show no value of a
   column the role cannot read.
5. `SET LOCAL` and `DISCARD ALL` leave no session option, prepared statement, cursor, advisory
   lock or `LISTEN` on a pooled connection. It also shows what they do not cover (findings B
   and C).
6. The SQL and the Rust pseudonym agree on 29 values (text, integers, numeric, floats, times,
   dates, bytea, booleans, UUIDs) while the SQL session has hostile settings (`TimeZone`,
   `DateStyle`, `extra_float_digits`, `bytea_output`, `IntervalStyle`).
7. Which extensions of the manifest allow-list `ruum_dbhost` can create, and how a lens kind
   reaches them.
8. What an untrusted migration (the `owner` kind) can and cannot do to the lens.

Last run (2026-11-07, PostgreSQL 17.11 on the build server): 225 passed, 0 failed.
