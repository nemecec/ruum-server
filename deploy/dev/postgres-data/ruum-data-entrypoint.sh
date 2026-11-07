#!/usr/bin/env bash
# Writes pg_hba.conf of the data cluster, then starts the official entrypoint.
#
# The per-database roles (r_ + 16 hex) and ruum_dbhost may log in only with SCRAM and only from
# the platform's address. On the laptop the platform runs on the host, and a connection to the
# published port reaches the container from the gateway of its network, so that address is the
# default. RUUM_DATA_PLATFORM_CIDR overrides it (one IPv4 address or CIDR).
# The superuser logs in only on the local socket (docker exec). Everything else is rejected.
set -euo pipefail

platform="${RUUM_DATA_PLATFORM_CIDR:-}"
if [[ -z $platform ]]; then
  gw="$(awk 'NR > 1 && $2 == "00000000" {print $3; exit}' /proc/net/route)"
  if [[ -z $gw ]]; then
    echo "ruum-data: no default route; set RUUM_DATA_PLATFORM_CIDR" >&2
    exit 1
  fi
  # /proc/net/route has the address in little-endian hex.
  platform="$(printf '%d.%d.%d.%d/32' "0x${gw:6:2}" "0x${gw:4:2}" "0x${gw:2:2}" "0x${gw:0:2}")"
fi
[[ $platform == */* ]] || platform="$platform/32"
if ! [[ $platform =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}/[0-9]{1,2}$ ]]; then
  echo "ruum-data: RUUM_DATA_PLATFORM_CIDR is not an IPv4 address or CIDR: $platform" >&2
  exit 1
fi

hba=/etc/postgresql/ruum/pg_hba.conf
cat >"$hba" <<HBA
# Written by ruum-data-entrypoint.sh at container start. Do not edit.
# TYPE  DATABASE  USER                  ADDRESS        METHOD
local   all       postgres                             trust
local   all       all                                  reject
host    all       ruum_dbhost           $platform  scram-sha-256
host    all       "/^r_[0-9a-f]{16}\$"  $platform  scram-sha-256
host    all       all                   0.0.0.0/0      reject
host    all       all                   ::/0           reject
HBA
chmod 0644 "$hba"
echo "ruum-data: per-database roles and ruum_dbhost may log in from $platform (SCRAM)" >&2

exec docker-entrypoint.sh "$@"
