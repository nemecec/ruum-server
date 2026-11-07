#!/usr/bin/env bash
# The role variables (ROLE_<key>, PW_<key>, DB_<key>) are set with eval and read with ${!name},
# and the conditions of `fact` are evaluated later, so they are in single quotes.
# shellcheck disable=SC2016,SC2034,SC2154
#
# Checks the data cluster postgres-data (docs/STEP2.md "Infrastructure work" 1 and 2, decision
# 0004-34 d and g, card I-1). It prints PASS or FAIL per check and exits non-zero on a failure.
#
#   scripts/dbhost_check.sh
#
# Settings:
#   RUUM_DBHOST_URL         the ruum_dbhost URL (default: from `scripts/dev.sh env`)
#   DBHOST_CHECK_CONTAINER  the postgres-data container (default: the one of compose project ruum)
#
# It needs docker, psql and jq. It uses the cluster as the platform does: ruum_dbhost (no
# superuser) creates two databases and the roles of every kind with neutral names, and every
# untrusted statement runs in a session that LOGS IN as the per-database role over TCP, with a
# short-lived password (never ruum_dbhost with SET ROLE). The superuser (docker exec, local
# socket) only looks at results. At the end it drops what it made, also after a failure.
# The first part of the checks is from spikes/step2-roles/spike.sh sections 1, 7 and 8.
# It runs with the bash 3.2 of macOS.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

die() { printf 'error: %s\n' "$*" >&2; exit 2; }
for c in docker psql jq; do command -v "$c" >/dev/null 2>&1 || die "$c is required"; done

URL="${RUUM_DBHOST_URL:-}"
if [[ -z $URL ]]; then
  URL="$("$ROOT/scripts/dev.sh" env | sed -n 's/^export RUUM_DBHOST_URL=//p')"
fi
re='^postgres(ql)?://([^:@/]+):([^@/]*)@([^:/]+):([0-9]+)/([^?]+)'
[[ $URL =~ $re ]] || die "RUUM_DBHOST_URL is not postgres://user:password@host:port/db"
DBHOST_USER=${BASH_REMATCH[2]}
DBHOST_PW=${BASH_REMATCH[3]}
HOST=${BASH_REMATCH[4]}
PORT=${BASH_REMATCH[5]}
ADMIN_DB=${BASH_REMATCH[6]}
[[ $DBHOST_USER == ruum_dbhost ]] || die "RUUM_DBHOST_URL must name the role ruum_dbhost"
[[ -n $DBHOST_PW ]] || die "RUUM_DBHOST_URL has no password; run scripts/dev.sh up"

PGC="${DBHOST_CHECK_CONTAINER:-$(docker ps -q --filter label=com.docker.compose.project=ruum \
  --filter label=com.docker.compose.service=postgres-data | head -1)}"
[[ -n $PGC ]] || die "no postgres-data container; run scripts/dev.sh up or set DBHOST_CHECK_CONTAINER"

ALLOW_LIST="$(jq -r '[.. | objects | select(.enum? and (.enum | index("postgis"))) | .enum][0][]' \
  "$ROOT/contracts/manifests/database.schema.json")"
[[ -n $ALLOW_LIST ]] || die "no extension allow-list in contracts/manifests/database.schema.json"

PASS=0
FAIL=0
FAILED=""
OUT=""
hex16() { od -An -N8 -tx1 /dev/urandom | tr -d ' \n'; }
newpw() { od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; }

pass() { PASS=$((PASS + 1)); printf 'PASS  %s\n' "$1"; }
fail() {
  FAIL=$((FAIL + 1))
  FAILED="$FAILED  failed: $1"$'\n'
  printf 'FAIL  %s\n' "$1"
  [[ -z $OUT ]] || printf '      | %s\n' "${OUT//$'\n'/$'\n      | '}"
}
# fact NAME COND: a check on OUT done by the caller.
fact() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }

# The superuser on the local socket of the container: set-up checks and inspection only.
su_sql() { # db sql
  OUT="$(docker exec -i "$PGC" psql -X -q -At -v ON_ERROR_STOP=1 -U postgres -d "$1" -c "$2" 2>&1)"
}

# A session that logs in over TCP as a role with its password (SCRAM), as the engine does.
# Roles are named by a key: the variable ROLE_<key> holds the name and PW_<key> the password.
as() { # key db sql  -> OUT, status
  local name="ROLE_$1" pwv="PW_$1"
  OUT="$(PGPASSWORD="${!pwv}" PGCONNECT_TIMEOUT=10 psql -X -q -At -v ON_ERROR_STOP=1 \
    -h "$HOST" -p "$PORT" -U "${!name}" -d "$2" -c "$3" 2>&1)"
}
ROLE_dbhost=$DBHOST_USER
PW_dbhost=$DBHOST_PW

# ok NAME KEY DB SQL [EXPECTED]: the statement succeeds (and prints EXPECTED).
ok() {
  if as "$2" "$3" "$4"; then
    if [[ $# -ge 5 && "$OUT" != "$5" ]]; then fail "$1 (want: $5)"; else pass "$1"; fi
  else fail "$1"; fi
}
# err NAME KEY DB SQL REGEX: the statement fails with a message that matches REGEX.
err() {
  if as "$2" "$3" "$4"; then fail "$1 (it ran)"
  elif grep -Eq "$5" <<<"$OUT"; then pass "$1"
  else fail "$1 (other error; want /$5/)"; fi
}
# same NAME KEY DB SQL: the statement fails, or it runs and current_user is still the role.
# The SQL ends with `select current_user`.
same() {
  local name="ROLE_$2"
  if as "$2" "$3" "$4"; then
    if [[ $(tail -1 <<<"$OUT") == "${!name}" ]]; then pass "$1 (runs, current_user unchanged)"
    else fail "$1 (current_user changed)"; fi
  elif grep -Eq 'permission denied|cannot set parameter' <<<"$OUT"; then pass "$1 (refused)"
  else fail "$1 (other error)"; fi
}

LOGIN_KINDS="owner read write masked_read masked_write secret_read"
MADE_DBS=""
MADE_ROLES=""

cleanup() {
  local d r
  for d in $MADE_DBS; do as dbhost "$ADMIN_DB" "drop database if exists $d with (force)" || echo "cleanup: $OUT" >&2; done
  for r in $MADE_ROLES; do as dbhost "$ADMIN_DB" "drop role if exists $r" || echo "cleanup: $OUT" >&2; done
}
trap cleanup EXIT

echo "== the cluster (as the superuser, inspection only)"
su_sql postgres "show server_version_num"
fact "PostgreSQL 17" '[[ $OUT == 17* ]]'
su_sql postgres "show max_prepared_transactions"
fact "max_prepared_transactions is 0 (decision 0004-34 d)" '[[ $OUT == 0 ]]'
su_sql postgres "select current_setting('wal_level') || '|' || current_setting('archive_mode')"
fact "WAL archiving is on (wal_level replica, archive_mode on)" '[[ $OUT == "replica|on" ]]'
su_sql postgres "select current_setting('shared_preload_libraries') !~ 'pg_stat_statements' and not exists (select 1 from pg_extension where extname = 'pg_stat_statements')"
fact "pg_stat_statements is not installed" '[[ $OUT == t ]]'
su_sql template1 "select coalesce(string_agg(extname, ','), '') from pg_extension where extname <> 'plpgsql'"
fact "template1 holds no extension (only the built-in plpgsql)" '[[ -z $OUT ]]'
su_sql postgres "select string_agg(name || '=' || trusted, ',' order by name) from pg_available_extension_versions v join pg_available_extensions a using (name) where name in ('postgis', 'vector') and v.version = a.default_version"
fact "postgis and vector are marked trusted (decision 0004-34 g)" '[[ $OUT == "postgis=true,vector=true" ]]'
su_sql postgres "select count(*) from pg_database d, aclexplode(coalesce(d.datacl, acldefault('d', d.datdba))) a where d.datname in ('postgres', 'template1') and a.grantee = 0 and a.privilege_type in ('CONNECT', 'TEMPORARY')"
fact "PUBLIC has no CONNECT and no TEMPORARY on postgres and template1" '[[ $OUT == 0 ]]'
su_sql template1 "select count(*) from pg_namespace n, aclexplode(coalesce(n.nspacl, acldefault('n', n.nspowner))) a where n.nspname = 'public' and a.grantee = 0"
fact "template1: PUBLIC has no right on schema public" '[[ $OUT == 0 ]]'
su_sql postgres "select rolsuper, rolcreatedb, rolcreaterole, rolreplication, rolbypassrls, rolpassword like 'SCRAM-SHA-256\$%' from pg_authid where rolname = 'ruum_dbhost'"
fact "ruum_dbhost: CREATEDB and CREATEROLE, no superuser, no replication, no bypassrls, SCRAM password" '[[ $OUT == "f|t|t|f|f|t" ]]'
su_sql postgres "select count(*) from pg_auth_members m join pg_roles r on r.oid = m.member where r.rolname = 'ruum_dbhost'"
fact "ruum_dbhost is a member of no role" '[[ $OUT == 0 ]]'

echo "== the network"
OUT="$(docker inspect -f '{{range $p, $b := .HostConfig.PortBindings}}{{range $b}}{{$p}}={{.HostIp}} {{end}}{{end}}' "$PGC")"
OTHER="$(tr ' ' '\n' <<<"$OUT" | grep . | grep -cv '=127\.0\.0\.1$')"
fact "every host port of postgres-data binds to 127.0.0.1 ($OUT)" '[[ -n $OUT && $OTHER == 0 ]]'
su_sql postgres "select count(*) from pg_hba_file_rules where error is not null"
fact "pg_hba.conf has no error" '[[ $OUT == 0 ]]'
su_sql postgres "select count(*) from pg_hba_file_rules where type like 'host%' and auth_method not in ('scram-sha-256', 'reject')"
fact "pg_hba.conf: TCP logins only with SCRAM (no trust, password or md5)" '[[ $OUT == 0 ]]'
su_sql postgres "select count(*) from pg_hba_file_rules where type like 'host%' and auth_method <> 'reject' and (user_name @> '{all}' or user_name @> '{postgres}')"
fact "pg_hba.conf: TCP logins only for ruum_dbhost and the per-database roles" '[[ $OUT == 0 ]]'
OUT="$(PGPASSWORD=x PGCONNECT_TIMEOUT=10 psql -X -At -h "$HOST" -p "$PORT" -U postgres -d postgres -c 'select 1' 2>&1)"
fact "the superuser cannot log in over TCP" 'grep -Eq "pg_hba.conf rejects|no pg_hba.conf entry|password authentication failed" <<<"$OUT"'
ok "ruum_dbhost logs in from the platform address with SCRAM" dbhost "$ADMIN_DB" "select current_user" ruum_dbhost

VALID_UNTIL=""
as dbhost "$ADMIN_DB" "select to_char((now() + interval '10 minutes') at time zone 'UTC', 'YYYY-MM-DD HH24:MI:SS') || '+00'" && VALID_UNTIL=$OUT
[[ -n $VALID_UNTIL ]] || { fail "ruum_dbhost: read the time"; exit 1; }

# make_db PREFIX: as ruum_dbhost, a database with a neutral name and the roles of every kind:
# LOGIN NOINHERIT with a password VALID UNTIL, CONNECT on their own database only; mask NOLOGIN.
make_db() {
  local p=$1 k db r
  db="d_$(hex16)"
  eval "DB_$p=\$db"
  if as dbhost "$ADMIN_DB" "create database $db"; then MADE_DBS="$MADE_DBS $db"
  else fail "$p: ruum_dbhost creates a database"; return 1; fi
  as dbhost "$ADMIN_DB" "revoke connect, temporary on database $db from public" || fail "$p: revoke from public"
  for k in $LOGIN_KINDS; do
    r="r_$(hex16)"
    eval "ROLE_${p}_$k=\$r; PW_${p}_$k=\$(newpw)"
    local pwv="PW_${p}_$k"
    if as dbhost "$ADMIN_DB" "create role $r login noinherit password '${!pwv}' valid until '$VALID_UNTIL'"; then
      MADE_ROLES="$MADE_ROLES $r"
      as dbhost "$ADMIN_DB" "grant connect on database $db to $r" || fail "$p: grant connect to $k"
    else fail "$p: ruum_dbhost creates role $k"; fi
  done
  r="r_$(hex16)"
  eval "ROLE_${p}_mask=\$r"
  if as dbhost "$ADMIN_DB" "create role $r nologin noinherit"; then MADE_ROLES="$MADE_ROLES $r"
  else fail "$p: ruum_dbhost creates role mask"; fi
  # Decision 0004-34 (a): ruum_dbhost holds SET (not INHERIT) membership in mask.
  as dbhost "$ADMIN_DB" "grant $r to ruum_dbhost with inherit false, set true" || fail "$p: grant mask to ruum_dbhost"
}

echo "== databases and roles (as ruum_dbhost, no superuser)"
if ! make_db A || ! make_db B; then echo "cannot go on" >&2; exit 1; fi
DBA=$DB_A
DBB=$DB_B
pass "ruum_dbhost created two databases and 2 x 7 roles with neutral names"
ALL_ROLES="'${MADE_ROLES# }'"
ALL_ROLES=${ALL_ROLES// /\',\'}
su_sql "$DBA" "select coalesce(string_agg(extname, ','), '') from pg_extension where extname <> 'plpgsql'"
fact "a new database holds no extension" '[[ -z $OUT ]]'
su_sql postgres "select count(*) from pg_auth_members m join pg_roles r on r.oid = m.member where r.rolname in ($ALL_ROLES)"
fact "every per-database role is a member of no role" '[[ $OUT == 0 ]]'
su_sql postgres "select count(*) from pg_roles where rolname in ($ALL_ROLES) and (rolinherit or rolsuper or rolcreaterole or rolcreatedb or rolreplication or rolbypassrls)"
fact "per-database roles: NOINHERIT, no SUPERUSER, CREATEROLE, CREATEDB, REPLICATION or BYPASSRLS" '[[ $OUT == 0 ]]'
su_sql postgres "select count(*) from pg_authid where rolname in ($ALL_ROLES) and rolcanlogin and (rolpassword not like 'SCRAM-SHA-256\$%' or rolvaliduntil is null)"
fact "per-database LOGIN roles: SCRAM password with VALID UNTIL" '[[ $OUT == 0 ]]'
su_sql postgres "select string_agg(distinct format('%s/%s/%s', admin_option, inherit_option, set_option), ',') from pg_auth_members m join pg_roles g on g.oid = m.roleid join pg_roles r on r.oid = m.member where g.rolname in ($ALL_ROLES) and g.rolcanlogin and r.rolname = 'ruum_dbhost'"
fact "ruum_dbhost holds ADMIN only (no INHERIT, no SET) on the LOGIN roles it made" '[[ $OUT == "t/f/f" ]]'
ok "ruum_dbhost: schema public of database A for the owner kind" dbhost "$DBA" "revoke all on schema public from public; grant usage, create on schema public to $ROLE_A_owner"

echo "== no per-database role reaches another role (each session LOGS IN as the role)"
for k in $LOGIN_KINDS; do
  key="A_$k"
  other=A_read
  [[ $k == read ]] && other=A_owner
  oname="ROLE_$other"
  me="ROLE_$key"
  ok "($k) session_user = current_user = the role itself" "$key" "$DBA" "select session_user = current_user and current_user = '${!me}'" t
  same "($k) RESET ROLE" "$key" "$DBA" "reset role; select current_user"
  for target in "$other" B_owner A_mask dbhost $([[ $k == masked_* ]] && echo A_owner); do
    tname="ROLE_$target"
    t=${!tname}
    same "($k) SET ROLE <$target>" "$key" "$DBA" "set role $t; select current_user"
    same "($k) set_config('role', <$target>, false)" "$key" "$DBA" "select set_config('role', '$t', false); select current_user"
    same "($k) DO block: SET ROLE <$target>" "$key" "$DBA" "do \$\$ begin execute 'set role $t'; end \$\$; select current_user"
    same "($k) DO block: set_config('role', <$target>, false)" "$key" "$DBA" "do \$\$ begin perform set_config('role', '$t', false); end \$\$; select current_user"
  done
  same "($k) SET ROLE postgres (the superuser)" "$key" "$DBA" "set role postgres; select current_user"
  same "($k) SET SESSION AUTHORIZATION <$other>" "$key" "$DBA" "set session authorization ${!oname}; select current_user"
  err "($k) cannot connect to database B" "$key" "$DBB" "select 1" 'permission denied for database|not have CONNECT'
  err "($k) cannot connect to postgres" "$key" postgres "select 1" 'permission denied for database|not have CONNECT'
  err "($k) cannot connect to template1" "$key" template1 "select 1" 'permission denied for database|not have CONNECT'
  err "($k) cannot make a TEMP table" "$key" "$DBA" "create temp table x (i int)" 'permission denied to create temporary tables'
  err "($k) cannot create a schema" "$key" "$DBA" "create schema evil" 'permission denied for database'
  err "($k) cannot create a database" "$key" "$DBA" "create database evil" 'permission denied to create database'
  err "($k) cannot create a role" "$key" "$DBA" "create role evil" 'permission denied to create role'
  err "($k) cannot PREPARE TRANSACTION (decision 0004-34 d)" "$key" "$DBA" "begin; prepare transaction 'p'" 'prepared transactions are disabled'
done
for k in $LOGIN_KINDS; do
  err "(B $k) cannot connect to database A" "B_$k" "$DBA" "select 1" 'permission denied for database|not have CONNECT'
done
ok "(owner) creates a SECURITY DEFINER function and a plain one" A_owner "$DBA" "
  create function public.sw() returns text language plpgsql security definer
    as \$f\$ begin perform set_config('role', '$ROLE_A_read', false); return current_user; end \$f\$;
  create function public.sw2() returns text language plpgsql
    as \$f\$ begin execute 'set role ruum_dbhost'; return current_user; end \$f\$;"
err "(owner) set_config('role') inside a SECURITY DEFINER function" A_owner "$DBA" "select public.sw()" 'cannot set parameter "role" within security-definer function|permission denied to set role'
err "(owner) SET ROLE ruum_dbhost inside a function" A_owner "$DBA" "select public.sw2()" 'permission denied to set role'

echo "== logins only from the platform address"
CIP="$(docker exec "$PGC" hostname -i | awk '{print $1}')"
OUT="$(docker exec -e PGPASSWORD="$PW_A_owner" -e PGCONNECT_TIMEOUT=10 "$PGC" psql -X -At -h "$CIP" -U "$ROLE_A_owner" -d "$DBA" -c 'select 1' 2>&1)"
fact "a per-database role cannot log in from another address ($CIP)" 'grep -Eq "pg_hba.conf rejects|no pg_hba.conf entry" <<<"$OUT"'
OUT="$(docker exec -e PGPASSWORD="$DBHOST_PW" -e PGCONNECT_TIMEOUT=10 "$PGC" psql -X -At -h "$CIP" -U ruum_dbhost -d "$ADMIN_DB" -c 'select 1' 2>&1)"
fact "ruum_dbhost cannot log in from another address ($CIP)" 'grep -Eq "pg_hba.conf rejects|no pg_hba.conf entry" <<<"$OUT"'

echo "== extensions of the manifest allow-list (as ruum_dbhost, no superuser)"
make_db C || exit 1
DBC=$DB_C
ok "ruum_dbhost creates the schema extensions" dbhost "$DBC" "create schema extensions"
for e in $ALLOW_LIST; do
  ok "ruum_dbhost creates the extension $e in schema extensions" dbhost "$DBC" "create extension \"$e\" schema extensions"
done
su_sql "$DBC" "select count(*) from pg_extension where extname <> 'plpgsql' and extnamespace <> 'extensions'::regnamespace"
fact "every extension is in schema extensions" '[[ $OUT == 0 ]]'
err "(owner) cannot create an extension (trusted pgcrypto, in database A)" A_owner "$DBA" "create extension pgcrypto" 'permission denied'
err "(masked_write) without USAGE on schema extensions: no extension function" C_masked_write "$DBC" "select extensions.similarity('abc', 'abd') > 0" 'permission denied for schema extensions'
ok "ruum_dbhost grants USAGE on schema extensions" dbhost "$DBC" "grant usage on schema extensions to $ROLE_C_masked_write"
ok "(masked_write) with USAGE: pg_trgm similarity()" C_masked_write "$DBC" "select extensions.similarity('abc', 'abd') > 0" t
ok "(masked_write) with USAGE: vector distance" C_masked_write "$DBC" "select extensions.l2_distance('[1,2]'::extensions.vector, '[1,3]'::extensions.vector)" 1
ok "(masked_write) with USAGE: PostGIS" C_masked_write "$DBC" "select extensions.st_astext(extensions.st_makepoint(1, 2))" 'POINT(1 2)'

echo "== the owner kind (untrusted migrations) cannot widen another kind"
as dbhost "$DBA" "grant usage on schema public to $ROLE_A_secret_read" || fail "dbhost: usage for secret_read"
ok "(owner) creates a table" A_owner "$DBA" "create table t (v text); insert into t values ('row of A')"
as A_owner "$DBA" "grant usage on schema public to $ROLE_A_masked_write"
fact "(owner) GRANT USAGE on schema public grants nothing (ruum_dbhost owns it)" 'grep -q "no privileges were granted" <<<"$OUT"'
su_sql "$DBA" "select has_schema_privilege('$ROLE_A_masked_write', 'public', 'usage')"
fact "(owner) masked_write still has no USAGE on public" '[[ $OUT == f ]]'
ok "(owner) may GRANT SELECT on its table to masked_write" A_owner "$DBA" "grant select on t to $ROLE_A_masked_write"
err "(masked_write) that grant is useless without USAGE on public" A_masked_write "$DBA" "select v from public.t" 'permission denied for schema public'
err "(owner) cannot ALTER the default privileges of mask" A_owner "$DBA" "alter default privileges for role $ROLE_A_mask grant select on tables to $ROLE_A_masked_write" 'permission denied|must be member'
err "(owner) cannot GRANT another role to itself" A_owner "$DBA" "grant $ROLE_A_mask to $ROLE_A_owner" 'permission denied|must have admin option'
err "(B owner) cannot read database A" B_owner "$DBA" "select * from public.t" 'permission denied for database|not have CONNECT'

echo "== result: $PASS passed, $FAIL failed"
printf '%s' "$FAILED"
[[ $FAIL -eq 0 ]]
