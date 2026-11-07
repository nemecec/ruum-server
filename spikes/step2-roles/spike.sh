#!/usr/bin/env bash
# Spike S-1 (docs/STEP2.md): prove the Postgres engine security model before the role kinds of
# `database_roles` freeze. See README.md next to this file.
#
#   scripts/buildbox/run.sh <name> <worktree> -- spikes/step2-roles/spike.sh
#
# It needs a Postgres 17 container that it may change (default: the agent Postgres of run.sh,
# pg-$BUILDBOX_NAME; or S1_PG_CONTAINER), docker, psql and cargo. The container's superuser is
# used ONLY for the cluster set-up of card I-1 (the role ruum_dbhost, REVOKE CONNECT on the
# shared databases) and to look at results; all platform DDL runs as ruum_dbhost, and every
# untrusted statement runs in a session that LOGS IN as the per-database role.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
PGC="${S1_PG_CONTAINER:-pg-${BUILDBOX_NAME:?set BUILDBOX_NAME or S1_PG_CONTAINER}}"
PORT="$(docker port "$PGC" 5432/tcp | head -1 | awk -F: '{print $NF}')"
[[ -n $PORT ]] || { echo "no port for $PGC" >&2; exit 2; }

PASS=0
FAIL=0
FAILED=()
hex16() { od -An -N8 -tx1 /dev/urandom | tr -d ' \n'; }
pw() { od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; }

declare -A R PW
TOKENS=()

# sub SQL: replace @name@ tokens with role names and other values.
sub() {
  local s=$1 k
  for k in "${!R[@]}"; do s=${s//@$k@/${R[$k]}}; done
  printf '%s' "$s"
}

# The cluster superuser (set-up of I-1 and inspection only).
su_sql() { # db sql
  docker exec -i "$PGC" psql -X -q -At -v ON_ERROR_STOP=1 -U postgres -d "$1" -c "$(sub "$2")"
}

# A session that logs in as a role with its password over TCP (SCRAM), as the engine does.
OUT=""
as() { # key db sql  -> OUT, status
  local key=$1 db=$2 sql
  sql="$(sub "$3")"
  OUT="$(PGPASSWORD="${PW[$key]}" psql -X -q -At -v ON_ERROR_STOP=1 -h 127.0.0.1 -p "$PORT" \
    -U "${R[$key]}" -d "$db" -c "$sql" 2>&1)"
}
# The same with a script on stdin (one connection, statements one by one, like a pooled session).
as_script() { # key db  (stdin: script)
  local key=$1 db=$2 sql
  sql="$(sub "$(cat)")"
  OUT="$(PGPASSWORD="${PW[$key]}" psql -X -q -At -h 127.0.0.1 -p "$PORT" \
    -U "${R[$key]}" -d "$db" -f - <<<"$sql" 2>&1)"
}

pass() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
fail() {
  FAIL=$((FAIL + 1))
  FAILED+=("$1")
  printf 'FAIL  %s\n' "$1"
  printf '      | %s\n' "${OUT//$'\n'/$'\n      | '}"
}

# ok NAME KEY DB SQL [EXPECTED]: the statement succeeds (and prints EXPECTED).
ok() {
  if as "$2" "$3" "$4"; then
    if [[ $# -ge 5 && "$OUT" != "$5" ]]; then fail "$1 (want: ${5//$'\n'/ | })"; else pass "$1"; fi
  else fail "$1"; fi
}
# err NAME KEY DB SQL REGEX: the statement fails with a message that matches REGEX.
err() {
  if as "$2" "$3" "$4"; then fail "$1 (it ran)"
  elif grep -Eq "$5" <<<"$OUT"; then pass "$1"
  else fail "$1 (other error; want /$5/)"; fi
}
# noleak NAME KEY DB SQL LIST...: succeeds and the output holds none of the raw values.
noleak() {
  local name=$1 key=$2 db=$3 sql=$4 v
  shift 4
  if ! as "$key" "$db" "$sql"; then fail "$name"; return; fi
  for v in "$@"; do
    if grep -qiF -- "$v" <<<"$OUT"; then fail "$name (shows '$v')"; return; fi
  done
  pass "$name"
}
# fact NAME COND: a check on OUT done by the caller.
fact() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }

RAW_PERSONAL=(alice@example.com bob@example.com carol@example.com "Alice Liddell" "Bob Builder" 1990-04-01 1985-02-03 2026-11-01)
RAW_SECRET=(tok_secret_alice tok_secret_bob tok_secret_carol)
KEY_HEX=000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f
LOGIN_KINDS=(owner read write masked_read masked_write secret_read)

echo "== cluster set-up (card I-1, as the superuser)"
su_sql postgres "select version()"
for d in $(su_sql postgres "select datname from pg_database where datname ~ '^d_[0-9a-f]{16}\$'"); do
  su_sql postgres "drop database \"$d\" with (force)" >/dev/null
done
for r in $(su_sql postgres "select rolname from pg_roles where rolname ~ '^r_[0-9a-f]{16}\$' or rolname = 'ruum_dbhost'"); do
  su_sql postgres "drop owned by \"$r\" cascade" >/dev/null 2>&1 || true
  su_sql postgres "drop role \"$r\"" >/dev/null
done
R[dbhost]=ruum_dbhost
PW[dbhost]=$(pw)
su_sql postgres "create role ruum_dbhost login createdb createrole password '${PW[dbhost]}'"
su_sql postgres "revoke connect, temporary on database postgres from public; revoke connect, temporary on database template1 from public; grant connect on database postgres to ruum_dbhost"
OUT="$(su_sql postgres "show createrole_self_grant")"
fact "PG17: createrole_self_grant is empty" '[[ -z $OUT ]]'
OUT="$(su_sql postgres "select rolsuper, rolcreatedb, rolcreaterole from pg_roles where rolname = 'ruum_dbhost'")"
fact "ruum_dbhost has CREATEDB and CREATEROLE, no superuser" '[[ $OUT == "f|t|t" ]]'

VALID_UNTIL="$(date -u -d '+5 min' '+%Y-%m-%d %H:%M:%S+00')"

# make_db PREFIX: as ruum_dbhost, a database with neutral names and the roles of every kind.
make_db() {
  local p=$1 k
  R[${p}db]="d_$(hex16)"
  as dbhost postgres "create database ${R[${p}db]}" || { fail "$p: create database"; return 1; }
  as dbhost postgres "revoke connect, temporary on database ${R[${p}db]} from public" || fail "$p: revoke"
  for k in "${LOGIN_KINDS[@]}"; do
    R[${p}$k]="r_$(hex16)"
    PW[${p}$k]=$(pw)
    as dbhost postgres "create role ${R[${p}$k]} login noinherit password '${PW[${p}$k]}' valid until '$VALID_UNTIL';
      grant connect on database ${R[${p}db]} to ${R[${p}$k]}" || fail "$p: role $k"
  done
  R[${p}mask]="r_$(hex16)"
  as dbhost postgres "create role ${R[${p}mask]} nologin noinherit" || fail "$p: role mask"
}

echo "== databases and roles (as ruum_dbhost)"
make_db ""
make_db B_
pass "ruum_dbhost made two databases and 2 x 7 roles with neutral names"
DBA=${R[db]}
DBB=${R[B_db]}

OUT="$(su_sql postgres "select count(*) from pg_auth_members m join pg_roles r on r.oid = m.member where r.rolname ~ '^r_'")"
fact "every per-database role is a member of no role" '[[ $OUT == 0 ]]'
OUT="$(su_sql postgres "select string_agg(distinct format('%s/%s/%s', admin_option, inherit_option, set_option), ',') from pg_auth_members m join pg_roles g on g.oid = m.roleid join pg_roles r on r.oid = m.member where g.rolname ~ '^r_' and r.rolname = 'ruum_dbhost'")"
fact "ruum_dbhost holds ADMIN only (no INHERIT, no SET) on the roles it made" '[[ $OUT == "t/f/f" ]]'
OUT="$(su_sql postgres "select count(*) from pg_roles where rolname ~ '^r_' and (rolinherit or rolsuper or rolcreaterole or rolcreatedb or rolreplication or rolbypassrls)")"
fact "per-database roles: NOINHERIT, no SUPERUSER/CREATEROLE/CREATEDB/REPLICATION/BYPASSRLS" '[[ $OUT == 0 ]]'

echo "== schemas, grants, the owner's tables (database A)"
as dbhost "$DBA" '
  revoke all on schema public from public;
  grant usage, create on schema public to @owner@;
  grant usage on schema public to @secret_read@, @mask@;
  create schema ruum_mask; create schema ruum_masked; create schema ruum_nosecret;
  grant usage, create on schema ruum_mask, ruum_masked, ruum_nosecret to @mask@;
  grant usage on schema ruum_nosecret to @read@, @write@;
  grant usage on schema ruum_masked to @masked_read@, @masked_write@;
  alter role @owner@ set search_path = public;
  alter role @secret_read@ set search_path = public;
  alter role @read@ set search_path = ruum_nosecret;
  alter role @write@ set search_path = ruum_nosecret;
  alter role @masked_read@ set search_path = ruum_masked;
  alter role @masked_write@ set search_path = ruum_masked;' &&
  pass "ruum_dbhost owns schema public and the ruum_* schemas; owner gets USAGE+CREATE on public only" ||
  fail "dbhost: schemas and grants"

# The platform, in the owner session, once: the base tables are readable by mask and secret_read.
ok "owner session: default privileges for mask and secret_read" owner "$DBA" '
  alter default privileges in schema public grant select, insert, update, delete on tables to @mask@;
  alter default privileges in schema public grant select on tables to @secret_read@'

# The first migration (untrusted SQL, owner kind).
ok "owner session: the first migration (tables and rows)" owner "$DBA" "
  create table users (
    id int primary key, email text not null, name text, plan text not null default 'free',
    age int check (age >= 0), code text, api_token text, born date, last_login timestamptz,
    unique (code, email));
  create table orders (id int primary key, user_email text, amount int not null default 0);
  insert into users values
    (1, 'alice@example.com', 'Alice Liddell', 'free', 30, 'c1', 'tok_secret_alice', '1990-04-01', '2026-11-01 10:00+00'),
    (2, 'bob@example.com', 'Bob Builder', 'pro', 41, 'c2', 'tok_secret_bob', '1985-02-03', '2026-11-02 11:00+00'),
    (3, 'carol@example.com', 'Carol', 'free', 25, 'c3', 'tok_secret_carol', '2000-12-24', null),
    (4, 'carol@example.com', 'Carol', 'free', 26, 'c4', null, null, null);
  insert into orders values (1, 'alice@example.com', 10), (2, 'bob@example.com', 20), (3, 'alice@example.com', 5);"
as dbhost "$DBB" "revoke all on schema public from public; grant usage, create on schema public to @B_owner@" || fail "db B: schema"
as B_owner "$DBB" "create table t (v text); insert into t values ('tenant B row')" || fail "db B: table"

echo "== objects owned by mask (as ruum_dbhost)"
# Without membership, ruum_dbhost cannot make an object owned by mask (PG16+ rule for OWNER TO).
err "finding (a): ruum_dbhost cannot give an object to mask without SET membership" dbhost "$DBA" '
  create table ruum_mask.probe (i int); alter table ruum_mask.probe owner to @mask@' 'must be able to SET ROLE|must be member'
ok "ruum_dbhost grants itself SET-only membership in mask (INHERIT FALSE)" dbhost "$DBA" '
  drop table if exists ruum_mask.probe; grant @mask@ to ruum_dbhost with inherit false, set true'

if as dbhost "$DBA" "
set role @mask@;
create table ruum_mask.key (id int primary key check (id = 1), ipad bytea not null, opad bytea not null);
insert into ruum_mask.key
  select 1,
    decode(string_agg(lpad(to_hex(get_byte(k, i) # 54), 2, '0'), '' order by i), 'hex'),
    decode(string_agg(lpad(to_hex(get_byte(k, i) # 92), 2, '0'), '' order by i), 'hex')
  from (select decode('$KEY_HEX' || repeat('00', 32), 'hex') as k) s, generate_series(0, 63) i;

create function ruum_mask.hmac12(t text) returns text language sql stable strict
  security definer set search_path = pg_catalog, pg_temp
  as \$\$ select 'p_' || left(encode(sha256(k.opad || sha256(k.ipad || convert_to(t, 'UTF8'))), 'hex'), 12)
       from ruum_mask.key k where k.id = 1 \$\$;

-- canonical_text: one overload per type, each with fixed settings, so no session option changes it.
create function ruum_mask.canonical(v text) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp as \$\$ select v \$\$;
create function ruum_mask.canonical(v bigint) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp as \$\$ select v::text \$\$;
create function ruum_mask.canonical(v numeric) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp as \$\$ select trim_scale(v)::text \$\$;
create function ruum_mask.canonical(v double precision) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp set extra_float_digits = 1 as \$\$ select v::text \$\$;
create function ruum_mask.canonical(v timestamptz) returns text language sql stable strict
  set search_path = pg_catalog, pg_temp set timezone = 'UTC'
  as \$\$ select case when isfinite(v)
       then rtrim(rtrim(to_char(v at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US'), '0'), '.') || 'Z'
       else v::text end \$\$;
create function ruum_mask.canonical(v date) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp
  as \$\$ select case when isfinite(v) then to_char(v, 'YYYY-MM-DD') else v::text end \$\$;
create function ruum_mask.canonical(v bytea) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp as \$\$ select encode(v, 'hex') \$\$;
create function ruum_mask.canonical(v boolean) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp as \$\$ select case when v then 'true' else 'false' end \$\$;
create function ruum_mask.canonical(v uuid) returns text language sql immutable strict
  set search_path = pg_catalog, pg_temp as \$\$ select v::text \$\$;

create function ruum_mask.pseudonym(v text) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v bigint) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v numeric) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v double precision) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v timestamptz) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v date) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v bytea) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v boolean) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
create function ruum_mask.pseudonym(v uuid) returns text language sql stable strict security definer
  set search_path = pg_catalog, pg_temp as \$\$ select ruum_mask.hmac12(ruum_mask.canonical(v)) \$\$;
revoke execute on all functions in schema ruum_mask from public;
grant execute on all functions in schema ruum_mask to @masked_read@, @masked_write@;
revoke execute on function ruum_mask.hmac12(text) from @masked_read@, @masked_write@;

create view ruum_masked.users as select id, ruum_mask.pseudonym(email) as email,
  ruum_mask.pseudonym(name) as name, plan, age, code,
  case when api_token is null then null else 'ruum:hidden'::text end as api_token,
  ruum_mask.pseudonym(born) as born, ruum_mask.pseudonym(last_login) as last_login
  from public.users;
create view ruum_masked.orders as select id, ruum_mask.pseudonym(user_email) as user_email, amount
  from public.orders;
create view ruum_nosecret.users as select id, email, name, plan, age, code,
  case when api_token is null then null else 'ruum:hidden'::text end as api_token, born, last_login
  from public.users;
create view ruum_nosecret.orders as select id, user_email, amount from public.orders;
grant select on all tables in schema ruum_masked to @masked_read@;
grant select, insert, update, delete on all tables in schema ruum_masked to @masked_write@;
grant select on all tables in schema ruum_nosecret to @read@;
grant select, insert, update, delete on all tables in schema ruum_nosecret to @write@;
reset role;"; then
  pass "ruum_dbhost (SET ROLE mask) built the key, the pseudonym functions and the lens views"
else fail "lens set-up as mask"; fi
su_sql "$DBA" "analyze public.users; analyze public.orders" >/dev/null

OUT="$(su_sql "$DBA" "select string_agg(distinct format('%s|%s|%s', p.prosecdef, pg_get_userbyid(p.proowner) = '@mask@', array_to_string(p.proconfig, ',')), ';') from pg_proc p where p.pronamespace = 'ruum_mask'::regnamespace and p.proname = 'pseudonym'")"
fact "pseudonym(): SECURITY DEFINER, owner mask, search_path=pg_catalog, pg_temp" '[[ $OUT == "t|t|search_path=pg_catalog, pg_temp" ]]'

echo "== (1) no role reaches another role"
for k in "${LOGIN_KINDS[@]}"; do
  other=read
  [[ $k == read ]] && other=owner
  ok "($k) session_user = current_user = the role itself" "$k" "$DBA" "select session_user = '@$k@' and current_user = '@$k@'" t
  ok "($k) RESET ROLE stays the role itself" "$k" "$DBA" "reset role; select current_user = '@$k@'" t
  for target in "$other" dbhost mask; do
    err "($k) SET ROLE <$target>" "$k" "$DBA" "set role @$target@" 'permission denied to set role'
    err "($k) set_config('role', <$target>)" "$k" "$DBA" "select set_config('role', '@$target@', false)" 'permission denied to set role'
    err "($k) DO block: SET ROLE <$target>" "$k" "$DBA" "do \$\$ begin execute 'set role @$target@'; end \$\$" 'permission denied to set role'
  done
  err "($k) SET ROLE postgres (superuser)" "$k" "$DBA" "set role postgres" 'permission denied to set role'
  err "($k) SET SESSION AUTHORIZATION <$other>" "$k" "$DBA" "set session authorization @$other@" 'permission denied to set session authorization'
  err "($k) cannot connect to database B" "$k" "$DBB" "select 1" 'permission denied for database|not have CONNECT'
  err "($k) cannot connect to postgres" "$k" postgres "select 1" 'permission denied for database|not have CONNECT'
  err "($k) cannot connect to template1" "$k" template1 "select 1" 'permission denied for database|not have CONNECT'
  err "($k) cannot make a TEMP table" "$k" "$DBA" "create temp table x (i int)" 'permission denied to create temporary tables'
  err "($k) cannot read ruum_mask.key" "$k" "$DBA" "select * from ruum_mask.key" 'permission denied for schema ruum_mask'
  err "($k) cannot call ruum_mask.pseudonym by name" "$k" "$DBA" "select ruum_mask.pseudonym('x'::text)" 'permission denied for schema ruum_mask'
done
ok "(owner) a SECURITY DEFINER function of the owner" owner "$DBA" '
  create function public.sw() returns text language plpgsql security definer
    as $f$ begin perform set_config($r$role$r$, $r$@read@$r$, false); return current_user; end $f$;
  create function public.sw2() returns text language plpgsql
    as $f$ begin execute $r$set role ruum_dbhost$r$; return current_user; end $f$;'
err "(owner) set_config('role') inside a SECURITY DEFINER function" owner "$DBA" "select public.sw()" 'cannot set parameter "role" within security-definer function|permission denied to set role'
err "(owner) SET ROLE ruum_dbhost inside a function" owner "$DBA" "select public.sw2()" 'permission denied to set role'
as dbhost "$DBA" "select 1" && pass "ruum_dbhost itself still connects (platform DDL path)"

echo "== (2) masked reads: pseudonyms in every clause; no base table, no key, no stats"
EXPECT_ALICE="$(cargo run -q --manifest-path "$HERE/pseudonym/Cargo.toml" <<<$'text\talice@example.com' | cut -f4)"
ok "masked_read: unqualified 'users' is the lens view" masked_read "$DBA" "select 'users'::regclass::text" "users"
ok "masked_read: search_path is ruum_masked" masked_read "$DBA" "show search_path" "ruum_masked"
ok "masked_read: email of row 1 is the pseudonym that Rust computes" masked_read "$DBA" "select email from users where id = 1" "$EXPECT_ALICE"
noleak "masked_read: select * (personal and secret masked)" masked_read "$DBA" "select * from users order by id" "${RAW_PERSONAL[@]}" "${RAW_SECRET[@]}"
ok "masked_read: secret column is the constant or NULL" masked_read "$DBA" "select coalesce(api_token, 'NULL') from users order by id" $'ruum:hidden\nruum:hidden\nruum:hidden\nNULL'
ok "masked_read: personal date and timestamptz columns are text pseudonyms" masked_read "$DBA" "select born like 'p\\_%' and last_login like 'p\\_%' and pg_typeof(born)::text = 'text' from users where id = 1" t
ok "masked_read: WHERE email LIKE 'a%' sees pseudonyms (0 rows)" masked_read "$DBA" "select count(*) from users where email like 'a%'" 0
ok "masked_read: WHERE email = <real value> (0 rows)" masked_read "$DBA" "select count(*) from users where email = 'alice@example.com'" 0
ok "masked_read: WHERE email = <pseudonym> (1 row)" masked_read "$DBA" "select count(*) from users where email = '$EXPECT_ALICE'" 1
noleak "masked_read: ORDER BY email" masked_read "$DBA" "select email from users order by email" "${RAW_PERSONAL[@]}"
noleak "masked_read: GROUP BY email, HAVING" masked_read "$DBA" "select email, count(*) from users group by email having count(*) > 0 order by 2 desc, 1" "${RAW_PERSONAL[@]}"
ok "masked_read: GROUP BY keeps equal values equal (carol twice)" masked_read "$DBA" "select max(c) from (select count(*) c from users group by email) s" 2
noleak "masked_read: min, max, string_agg" masked_read "$DBA" "select min(email), max(name), string_agg(email, ',') from users" "${RAW_PERSONAL[@]}"
ok "masked_read: length(email) is the pseudonym length" masked_read "$DBA" "select string_agg(distinct length(email)::text, ',') from users" 14
ok "masked_read: JOIN on personal columns matches pseudonyms" masked_read "$DBA" "select string_agg(u.id || ':' || o.amount, ',' order by u.id, o.amount) from users u join orders o on o.user_email = u.email" "1:5,1:10,2:20"
ok "masked_read: JOIN personal = normal column matches nothing" masked_read "$DBA" "select count(*) from users u join orders o on u.code = o.user_email" 0
ok "masked_read: subquery and CTE" masked_read "$DBA" "with x as (select email from users) select count(*) from x where email like '%@%' or email in (select 'alice@example.com')" 0
noleak "masked_read: EXPLAIN shows no value" masked_read "$DBA" "explain select * from users where email = 'zzz'" "${RAW_PERSONAL[@]}" "${RAW_SECRET[@]}"
noleak "masked_read: the view definitions" masked_read "$DBA" "select pg_get_viewdef('users'::regclass) || pg_get_viewdef('orders'::regclass)" "${RAW_PERSONAL[@]}" "${RAW_SECRET[@]}"
noleak "masked_read: function bodies in pg_proc hold no key" masked_read "$DBA" "select string_agg(prosrc, ' ') from pg_proc where pronamespace = 'ruum_mask'::regnamespace" "${KEY_HEX:0:12}" "${KEY_HEX:12:12}"
err "masked_read: cannot read public.users" masked_read "$DBA" "select * from public.users" 'permission denied for schema public'
err "masked_read: cannot read the base table through query_to_xml" masked_read "$DBA" "select query_to_xml('select * from public.users', true, true, '')" 'permission denied'
ok "masked_read: pg_stats shows no row of the base tables" masked_read "$DBA" "select count(*) from pg_stats where tablename in ('users', 'orders')" 0
ok "finding (g): the stack image has PostGIS in template1 (public.spatial_ref_sys in every new database)" masked_read "$DBA" "select count(*) > 0 from pg_stats where schemaname = 'public' and tablename = 'spatial_ref_sys'" t
err "masked_read: pg_statistic is closed" masked_read "$DBA" "select * from pg_statistic" 'permission denied'
OUT="$(su_sql "$DBA" "select count(*) from pg_stats where schemaname = 'public' and most_common_vals::text like '%carol@example.com%'")"
fact "(control) the superuser sees carol in pg_stats, so the stats exist" '[[ $OUT == 1 ]]'
noleak "masked_read: pg_seclabels and settings show no key" masked_read "$DBA" "select (select count(*) from pg_seclabels)::text || string_agg(setting, ',') from pg_settings" "${KEY_HEX:0:12}"
err "masked_read: cannot create a function (no schema with CREATE)" masked_read "$DBA" "create function f() returns int language sql as 'select 1'" 'no schema has been selected|permission denied'
err "masked_read: cannot create in ruum_masked" masked_read "$DBA" "create view ruum_masked.v as select 1" 'permission denied for schema ruum_masked'
noleak "read (nosecret): personal visible, secret masked" read "$DBA" "select * from users order by id" "${RAW_SECRET[@]}"
ok "read (nosecret): WHERE on the secret column sees the constant" read "$DBA" "select count(*) from users where api_token like 'tok%'" 0
ok "secret_read (base): sees the secret" secret_read "$DBA" "select api_token from users where id = 1" tok_secret_alice
noleak "owner: still no key" owner "$DBA" "select 1" "${KEY_HEX:0:12}"

echo "== (3) the pseudonym function runs from a view only"
ok "masked_read: the view calls pseudonym() (no schema USAGE needed)" masked_read "$DBA" "select count(*) from users where email like 'p\\_%'" 4
err "masked_read: hmac12 has no EXECUTE either" masked_read "$DBA" "select ruum_mask.hmac12('x')" 'permission denied'
ok "masked_read: SELECT 'guess' comes back as it is" masked_read "$DBA" "select 'alice@example.com'" alice@example.com

echo "== (4) masked_write: plain columns only; WHERE compares pseudonyms"
ok "masked_write: UPDATE a plain column" masked_write "$DBA" "update users set plan = 'pro' where id = 1 returning plan" pro
ok "masked_write: INSERT plain columns" masked_write "$DBA" "insert into orders (id, amount) values (10, 99) returning amount" 99
err "masked_write: UPDATE a masked personal column" masked_write "$DBA" "update users set email = 'x@y' where id = 1" 'cannot update column "email"'
err "masked_write: UPDATE the masked secret column" masked_write "$DBA" "update users set api_token = 'x' where id = 1" 'cannot update column "api_token"'
err "masked_write: INSERT into a masked column" masked_write "$DBA" "insert into orders (id, user_email) values (11, 'x@y')" 'cannot insert into column "user_email"'
ok "masked_write: WHERE email = <real value> matches nothing" masked_write "$DBA" "with u as (update users set plan = 'x' where email = 'alice@example.com' returning 1) select count(*) from u" 0
ok "masked_write: WHERE email LIKE 'a%' matches nothing" masked_write "$DBA" "with u as (update users set plan = 'x' where email like 'a%' returning 1) select count(*) from u" 0
ok "masked_write: WHERE email = <pseudonym> matches the row" masked_write "$DBA" "with u as (update users set plan = 'y' where email = '$EXPECT_ALICE' returning 1) select count(*) from u" 1
noleak "masked_write: RETURNING a personal column gives the pseudonym" masked_write "$DBA" "update users set plan = 'z' where id = 2 returning email, name, api_token" "${RAW_PERSONAL[@]}" "${RAW_SECRET[@]}"
ok "masked_write: SET plain = personal copies the pseudonym, never the value" masked_write "$DBA" "update users set code = email where id = 2 returning code = '$(cargo run -q --manifest-path "$HERE/pseudonym/Cargo.toml" <<<$'text\tbob@example.com' | cut -f4)'" t
OUT="$(su_sql "$DBA" "select code from users where id = 2")"
fact "(control) the base row holds the pseudonym, not bob's address" '[[ $OUT == p_* ]]'
as masked_write "$DBA" "update users set age = -1 where id = 1"
fact "masked_write: a CHECK violation shows no other column's value" '[[ $OUT == *users_age_check* ]] && ! grep -qF alice@example.com <<<"$OUT"'
as masked_write "$DBA" "update users set code = 'c3' where id = 4"
fact "masked_write: a UNIQUE (code, email) violation shows no key value" '[[ $OUT == *users_code_email_key* ]] && ! grep -qF carol@example.com <<<"$OUT"'
as write "$DBA" "update users set code = 'c3' where id = 4"
fact "write (nosecret): a UNIQUE violation shows no key value either" '[[ $OUT == *users_code_email_key* ]] && ! grep -qF carol@example.com <<<"$OUT"'
as owner "$DBA" "update users set code = 'c3' where id = 4"
fact "(control) owner sees the key value in DETAIL (people with manage get the engine message)" 'grep -qF carol@example.com <<<"$OUT"'
ok "write (nosecret): UPDATE a personal column" write "$DBA" "update users set name = 'Carol C' where id = 4 returning name" "Carol C"
err "write (nosecret): UPDATE the secret column" write "$DBA" "update users set api_token = 'x' where id = 1" 'cannot update column "api_token"'
err "read: no writes" read "$DBA" "update users set plan = 'x' where id = 1" 'permission denied'
err "masked_read: no writes" masked_read "$DBA" "update users set plan = 'x' where id = 1" 'permission denied'
err "secret_read: no writes" secret_read "$DBA" "update users set plan = 'x' where id = 1" 'permission denied'

echo "== (5) pooled sessions: SET LOCAL and DISCARD ALL"
as_script masked_read "$DBA" <<'EOF'
\echo fresh
show statement_timeout; show search_path; show timezone; show extra_float_digits; show transaction_read_only;
begin;
set local statement_timeout = '700ms';
set local transaction_read_only = on;
\echo inside
show statement_timeout; show transaction_read_only;
select pg_sleep(1.5);
rollback;
\echo after_local
show statement_timeout; show transaction_read_only;
select set_config('statement_timeout', '123s', false) is not null;
do $$ begin perform set_config('search_path', 'pg_catalog', false); perform set_config('timezone', 'Pacific/Chatham', false); end $$;
set extra_float_digits = -3;
prepare p as select 1;
declare c cursor with hold for select 1;
select pg_advisory_lock(42) is not null;
listen chan;
set session characteristics as transaction read only;
\echo dirty
show statement_timeout; show search_path; show timezone; show extra_float_digits;
discard all;
\echo clean
show statement_timeout; show search_path; show timezone; show extra_float_digits; show default_transaction_read_only;
select (select count(*) from pg_prepared_statements) || '/' || (select count(*) from pg_cursors) || '/' || (select count(*) from pg_locks where locktype = 'advisory' and pid = pg_backend_pid()) || '/' || (select count(*) from pg_listening_channels());
EOF
fresh="$(sed -n '/^fresh$/,/^inside$/p' <<<"$OUT" | sed '1d;$d' | head -4)"
clean="$(sed -n '/^clean$/,$p' <<<"$OUT" | sed '1d' | head -4)"
fact "SET LOCAL holds inside the transaction" 'sed -n "/^inside$/,/^after_local$/p" <<<"$OUT" | grep -q "^700ms$"'
fact "statement_timeout of SET LOCAL cancels a long statement" 'grep -q "canceling statement due to statement timeout" <<<"$OUT"'
fact "after the transaction, SET LOCAL leaves nothing" '[[ "$(sed -n "/^after_local$/,/^dirty$/p" <<<"$OUT" | sed -n 2p)" == "0" ]]'
fact "session options set by a hostile statement are there before DISCARD ALL" 'sed -n "/^dirty$/,/^clean$/p" <<<"$OUT" | grep -q "Pacific/Chatham"'
fact "DISCARD ALL restores every option of a fresh session" '[[ "$fresh" == "$clean" ]]'
fact "DISCARD ALL drops prepared statements, cursors, advisory locks and LISTEN" '[[ "$(tail -1 <<<"$OUT")" == "0/0/0/0" ]]'
fact "DISCARD ALL resets SET SESSION CHARACTERISTICS" 'sed -n "/^clean$/,\$p" <<<"$OUT" | sed -n 6p | grep -q "^off$"'

as_script masked_read "$DBA" <<'EOF'
begin;
select 1/0;
discard all;
EOF
fact "a failed transaction cannot be cleaned with DISCARD ALL (the pool must close it)" 'grep -q "current transaction is aborted" <<<"$OUT"'

as_script masked_read "$DBA" <<'EOF'
begin;
set local statement_timeout = '700ms';
do $$ begin perform set_config('statement_timeout', '0', true); perform pg_sleep(1.5); end $$;
rollback;
EOF
fact "a DO block that lifts statement_timeout cannot free its own statement" 'grep -q "canceling statement due to statement timeout" <<<"$OUT"'

as_script masked_read "$DBA" <<'EOF'
begin;
set local statement_timeout = '700ms';
do $$ begin commit; end $$;
rollback;
EOF
fact "COMMIT inside a DO block fails inside the platform's transaction" 'grep -q "invalid transaction termination" <<<"$OUT"'

as_script masked_read "$DBA" <<'EOF'
begin;
set local statement_timeout = '700ms';
commit;
select pg_sleep(1.2) is not null as ran_without_timeout;
EOF
fact "finding (d): a COMMIT statement ends the platform's transaction (next statement has no timeout)" 'grep -q "^t$" <<<"$OUT" && ! grep -q "statement timeout" <<<"$OUT"'

as_script owner "$DBA" <<'EOF'
begin;
create table mig_part1 (i int);
commit;
create table mig_part2 (i int);
select 1/0;
rollback;
EOF
OUT2="$OUT"
OUT="$(su_sql "$DBA" "select count(*) from pg_class where relname = 'mig_part1'")"
fact "finding (d): COMMIT inside a migration's 'up' breaks its one-transaction rollback" '[[ $OUT == 1 ]]'
su_sql "$DBA" "drop table if exists mig_part1, mig_part2" >/dev/null
err "PREPARE TRANSACTION is off (max_prepared_transactions = 0)" masked_read "$DBA" "begin; prepare transaction 'x'" 'prepared transactions are disabled|cannot'

ok "finding (c): a role can ALTER ROLE itself SET a session default" masked_read "$DBA" "alter role current_user set timezone = 'Pacific/Chatham'"
ok "finding (c): the default survives DISCARD ALL into the next connection" masked_read "$DBA" "show timezone" "Pacific/Chatham"
ok "finding (c): a role can change its own password" masked_read "$DBA" "alter role current_user password 'known'"
PW[masked_read]=known
ok "finding (c) fix: ruum_dbhost resets the role (RESET ALL, new password) before it opens connections" dbhost "$DBA" "alter role @masked_read@ reset all; alter role @masked_read@ set search_path = ruum_masked; alter role @masked_read@ password '$(pw)' valid until '$VALID_UNTIL'"
PW[masked_read]=known
err "finding (c) fix: the password the role chose no longer works" masked_read "$DBA" "select 1" 'password authentication failed'
OUT="$(su_sql "$DBA" "select coalesce(string_agg(array_to_string(setconfig, ','), ';'), '') from pg_db_role_setting where setrole = '@masked_read@'::regrole")"
fact "finding (c) fix: only the platform's search_path is left on the role" '[[ $OUT == "search_path=ruum_masked" ]]'

echo "== (6) the SQL and the Rust pseudonym agree"
RUST="$(cargo run -q --manifest-path "$HERE/pseudonym/Cargo.toml" <"$HERE/values.tsv")"
SQL="select * from (values "
i=0
while IFS=$'\t' read -r ty lit; do
  [[ $i -gt 0 ]] && SQL+=", "
  SQL+="($i, '$ty', '$lit', ruum_mask.canonical('$lit'::$ty), ruum_mask.pseudonym('$lit'::$ty))"
  i=$((i + 1))
done <"$HERE/values.tsv"
SQL+=") v(i, ty, lit, c, p) order by i"
PGSQL="$(docker exec -i "$PGC" psql -X -q -At -F $'\t' -U postgres -d "$DBA" <<EOF
set timezone = 'Pacific/Chatham';
set datestyle = 'SQL, DMY';
set extra_float_digits = -3;
set bytea_output = 'escape';
set intervalstyle = 'sql_standard';
select ty, lit, c, p from ($SQL) s;
EOF
)"
if [[ "$RUST" == "$PGSQL" ]]; then
  pass "Rust and SQL agree on $i values (text, numbers, floats, times, bytea, ...) under hostile session settings"
else
  OUT="$(diff <(echo "$RUST") <(echo "$PGSQL"))"
  fail "Rust and SQL pseudonyms differ"
fi
printf '%s\n' "$PGSQL" | sed 's/^/      /'
OUT="$(docker exec -i "$PGC" psql -X -q -At -U postgres -d "$DBA" <<'EOF'
set timezone = 'UTC';
select ruum_mask.pseudonym('2026-11-07T12:34:56+02:00'::timestamptz::text);
set timezone = 'Pacific/Chatham';
select ruum_mask.pseudonym('2026-11-07T12:34:56+02:00'::timestamptz::text);
select ruum_mask.pseudonym('2026-11-07T12:34:56+02:00'::timestamptz);
set timezone = 'UTC';
select ruum_mask.pseudonym('2026-11-07T12:34:56+02:00'::timestamptz);
EOF
)"
mapfile -t L <<<"$OUT"
fact "finding (e): pseudonym(v::text) depends on the session (TimeZone); the typed overloads do not" '[[ ${L[0]} != "${L[1]}" && ${L[2]} == "${L[3]}" ]]'

echo "== (7) extensions of the manifest allow-list, created by ruum_dbhost (not a superuser)"
R[cdb]="d_$(hex16)"
as dbhost postgres "create database ${R[cdb]} template template0" || fail "create database from template0"
as dbhost "${R[cdb]}" "create schema extensions" || fail "schema extensions"
for e in postgis vector pg_trgm citext hstore uuid-ossp pgcrypto btree_gist unaccent; do
  if as dbhost "${R[cdb]}" "create extension \"$e\" schema extensions"; then
    echo "      extension $e: created by ruum_dbhost"
  else
    echo "      extension $e: refused for ruum_dbhost: $(head -1 <<<"$OUT")"
  fi
done
as dbhost "${R[cdb]}" "create extension if not exists pg_trgm schema extensions" &&
  as dbhost "${R[cdb]}" "grant connect on database ${R[cdb]} to @masked_write@" &&
  as masked_write "${R[cdb]}" "select extensions.similarity('abc', 'abd') > 0"
fact "finding (g): a lens role reaches extension functions only with USAGE on their schema" 'grep -q "permission denied for schema extensions" <<<"$OUT"'
ok "finding (g): with USAGE on the schema 'extensions' it can use them" dbhost "${R[cdb]}" "grant usage on schema extensions to @masked_write@"
ok "finding (g): (masked_write) similarity() from schema extensions" masked_write "${R[cdb]}" "select extensions.similarity('abc', 'abd') > 0" t

echo "== (8) the owner kind (untrusted migrations) cannot widen a lens"
as owner "$DBA" "grant usage on schema public to @masked_write@"
fact "owner: GRANT USAGE on schema public grants nothing (ruum_dbhost owns it)" 'grep -q "no privileges were granted" <<<"$OUT"'
OUT="$(su_sql "$DBA" "select has_schema_privilege('@masked_write@', 'public', 'usage')")"
fact "owner: masked_write still has no USAGE on public" '[[ $OUT == f ]]'
ok "owner: may GRANT SELECT on its base table to masked_write" owner "$DBA" "grant select on users to @masked_write@"
err "masked_write: that grant is useless without USAGE on public" masked_write "$DBA" "select email from public.users" 'permission denied for schema public'
err "owner: cannot replace a lens view" owner "$DBA" "create or replace view ruum_masked.users as select * from public.users" 'permission denied for schema ruum_masked|must be owner'
err "owner: cannot create a schema" owner "$DBA" "create schema evil" 'permission denied for database'
err "owner: cannot create an extension" owner "$DBA" "create extension pgcrypto" 'permission denied'
err "owner: cannot ALTER the default privileges of mask" owner "$DBA" "alter default privileges for role @mask@ grant select on tables to @masked_write@" 'permission denied|must be member'
err "finding (f): a lens view blocks ALTER COLUMN TYPE in a migration" owner "$DBA" "alter table users alter column age type bigint" 'cannot alter type of a column used by a view or rule'
err "finding (f): a lens view blocks DROP COLUMN in a migration" owner "$DBA" "alter table users drop column name" 'other objects depend on it'
ok "finding (f): RENAME COLUMN works with the lens views in place" owner "$DBA" "alter table users rename column name to full_name"
OUT="$(su_sql "$DBA" "select pg_get_viewdef('ruum_masked.users'::regclass) like '%pseudonym(full_name) AS name%'")"
fact "finding (f): the lens view follows the renamed column (by attnum, not by name)" '[[ $OUT == t ]]'
as owner "$DBA" "drop table orders cascade"
fact "finding (f): DROP TABLE ... CASCADE by the owner drops mask's lens views too" 'grep -q "drop cascades to view ruum_masked.orders" <<<"$OUT"'
err "masked_write: the dropped lens view is gone (no fallback to anything)" masked_write "$DBA" "select * from orders" 'does not exist'

echo "== result: $PASS passed, $FAIL failed"
for f in "${FAILED[@]}"; do echo "  failed: $f"; done
[[ $FAIL -eq 0 ]]
