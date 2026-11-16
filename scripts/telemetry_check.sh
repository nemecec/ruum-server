#!/usr/bin/env bash
# Check the tenant isolation of the telemetry stack (card I-5, docs/DEVSTACK.md "Telemetry stack
# (profile apps)"). Start it first: scripts/dev.sh up --apps.
#
#   eval "$(scripts/dev.sh env)"; scripts/telemetry_check.sh
#
# For each signal (logs, metrics, traces) the script writes one record through the Collector's
# OTLP endpoint with the header X-Scope-OrgID: t-a and one with t-b, and reads each back from its
# store. A record must be found with its own tenant, not found with the other tenant, and the
# query without the header must be refused. A query with two tenants in the header ("t-b|t-a")
# must not find it either (cross-tenant queries are off). A write without the header must be
# refused too. It also checks that every published port of the stack binds to 127.0.0.1 only and
# that the stores and the Collector are on the network `telemetry` only.
#
# No check passes on silence: "not found" counts only after the same query found the record with
# its own tenant. The script waits for ingestion with a bounded retry (RUUM_TELEMETRY_TIMEOUT,
# default 60 seconds) and fails on the timeout. It prints PASS or FAIL per check and exits
# non-zero on any failure.
#
# Settings (from `scripts/dev.sh env`): RUUM_OTLP_ENDPOINT (OTLP HTTP), RUUM_LOKI_URL,
# RUUM_MIMIR_URL, RUUM_TEMPO_URL. COMPOSE_PROJECT_NAME selects the compose project (default ruum).
# Needs curl, jq, openssl and docker.
set -euo pipefail

OTLP="${RUUM_OTLP_ENDPOINT:-http://127.0.0.1:4318}"
LOKI="${RUUM_LOKI_URL:-http://127.0.0.1:3100}"
MIMIR="${RUUM_MIMIR_URL:-http://127.0.0.1:9009}"
TEMPO="${RUUM_TEMPO_URL:-http://127.0.0.1:3200}"
PROJECT="${COMPOSE_PROJECT_NAME:-ruum}"
TIMEOUT="${RUUM_TELEMETRY_TIMEOUT:-60}"
CURL_MAX=10 # seconds per request; the two-tenant query raises it (Loki retries before it refuses)
SERVICES=(loki mimir tempo otel-collector)
HEADER=X-Scope-OrgID

for tool in curl jq openssl docker; do
  command -v "$tool" >/dev/null 2>&1 || { echo "error: $tool is required" >&2; exit 2; }
done

failures=0
pass() { printf 'PASS %s\n' "$*"; }
fail() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }

# A random run ID, so a record of an earlier run never counts.
RUN="$(openssl rand -hex 8)"
now_ns() { printf '%s000000000' "$(date +%s)"; }

# http <method> <url> <tenant or ""> [curl args...]: prints the body, then the status code on the
# last line. The tenant "" sends no header.
http() {
  local method=$1 url=$2 tenant=$3
  shift 3
  local hdr=()
  [[ -n "$tenant" ]] && hdr=(-H "$HEADER: $tenant")
  curl -sS -m "$CURL_MAX" -X "$method" ${hdr[@]+"${hdr[@]}"} -w '\n%{http_code}' "$@" "$url" || printf '\n000'
}
status_of() { tail -n1 <<<"$1"; }
body_of() { sed '$d' <<<"$1"; }

# ---- writes ---------------------------------------------------------------------------------

otlp_post() { # <path> <tenant or ""> <json>
  http POST "$OTLP$1" "$2" -H 'Content-Type: application/json' --data-binary "$3"
}

log_json() { # <tenant> <nonce>
  jq -cn --arg nonce "$2" --arg ts "$(now_ns)" --arg tenant "$1" '{resourceLogs: [{
    resource: {attributes: [{key: "service.name", value: {stringValue: "ruum-telemetry-check"}}]},
    scopeLogs: [{scope: {name: "telemetry_check"}, logRecords: [{
      timeUnixNano: $ts, observedTimeUnixNano: $ts, severityNumber: 9, severityText: "INFO",
      body: {stringValue: ("telemetry check " + $nonce + " for " + $tenant)},
      attributes: [{key: "check_nonce", value: {stringValue: $nonce}}]}]}]}]}'
}

metric_json() { # <tenant> <nonce>
  jq -cn --arg nonce "$2" --arg ts "$(now_ns)" '{resourceMetrics: [{
    resource: {attributes: [{key: "service.name", value: {stringValue: "ruum-telemetry-check"}}]},
    scopeMetrics: [{scope: {name: "telemetry_check"}, metrics: [{
      name: "ruum_telemetry_check", description: "telemetry_check.sh",
      gauge: {dataPoints: [{timeUnixNano: $ts, asDouble: 1,
        attributes: [{key: "check_nonce", value: {stringValue: $nonce}}]}]}}]}]}]}'
}

span_json() { # <tenant> <nonce> <trace id> <span id>
  local start end
  start="$(now_ns)"
  end=$((start + 5000000))
  jq -cn --arg nonce "$2" --arg trace "$3" --arg span "$4" --arg start "$start" --arg end "$end" '{resourceSpans: [{
    resource: {attributes: [{key: "service.name", value: {stringValue: "ruum-telemetry-check"}}]},
    scopeSpans: [{scope: {name: "telemetry_check"}, spans: [{
      traceId: $trace, spanId: $span, name: "telemetry-check", kind: 1,
      startTimeUnixNano: $start, endTimeUnixNano: $end,
      attributes: [{key: "check_nonce", value: {stringValue: $nonce}}]}]}]}]}'
}

write_ok() { # <what> <path> <tenant> <json>
  local res code
  res="$(otlp_post "$2" "$3" "$4")"
  code="$(status_of "$res")"
  if [[ "$code" == 2* ]]; then
    pass "$1: write with $HEADER: $3 accepted ($code)"
  else
    fail "$1: write with $HEADER: $3 got $code: $(body_of "$res" | head -c 300)"
  fi
}

write_refused() { # <what> <path> <json>
  local res code
  res="$(otlp_post "$2" "" "$3")"
  code="$(status_of "$res")"
  # The stores refuse it with 401 (Tempo 400). Only a 4xx counts: a 5xx does not show that the
  # store refused the record (it may have stored it and failed later), and 000 is no answer.
  if [[ "$code" == 4* ]]; then
    pass "$1: write without $HEADER refused ($code)"
  else
    fail "$1: write without $HEADER got $code (want a 4xx refusal)"
  fi
}

# ---- reads ----------------------------------------------------------------------------------
# Each query function takes the tenant ("" for none) and the record; it prints the status code
# and "found" or "empty". After a 5xx it adds "refused" when the body is the store's refusal of a
# two-tenant header (Loki answers "multiple org IDs present" with 500).
refusal() { # <response>: prints " refused" for a 5xx whose body refuses a two-tenant header
  [[ "$(status_of "$1")" == 5* ]] && body_of "$1" | grep -qiE 'multiple org ids|too many tenant ids' && printf ' refused'
  return 0
}

query_log() { # <tenant> <nonce>
  local q res code n
  q="{service_name=\"ruum-telemetry-check\"} |= \"$2\""
  res="$(http GET "$LOKI/loki/api/v1/query_range" "$1" -G --data-urlencode "query=$q" \
    --data-urlencode "start=$(($(date +%s) - 600))000000000" --data-urlencode "end=$(($(date +%s) + 60))000000000" \
    --data-urlencode limit=10)"
  code="$(status_of "$res")"
  n="$(body_of "$res" | jq -r --arg nonce "$2" '[.data.result[]?.values[]?[1] | select(contains($nonce))] | length' 2>/dev/null || echo 0)"
  printf '%s %s%s\n' "$code" "$([[ "${n:-0}" -gt 0 ]] && echo found || echo empty)" "$(refusal "$res")"
}

query_metric() { # <tenant> <nonce>
  local res code n
  res="$(http GET "$MIMIR/prometheus/api/v1/query" "$1" -G \
    --data-urlencode "query=ruum_telemetry_check{check_nonce=\"$2\"}")"
  code="$(status_of "$res")"
  n="$(body_of "$res" | jq -r '.data.result | length' 2>/dev/null || echo 0)"
  printf '%s %s%s\n' "$code" "$([[ "${n:-0}" -gt 0 ]] && echo found || echo empty)" "$(refusal "$res")"
}

query_trace() { # <tenant> <trace id>
  local res code n
  res="$(http GET "$TEMPO/api/v2/traces/$2" "$1")"
  code="$(status_of "$res")"
  n="$(body_of "$res" | jq -r '[(.trace.resourceSpans // .batches // [])[]?.scopeSpans[]?.spans[]?] | length' 2>/dev/null || echo 0)"
  printf '%s %s%s\n' "$code" "$([[ "${n:-0}" -gt 0 ]] && echo found || echo empty)" "$(refusal "$res")"
}

# isolated <what> <query function> <record> <own tenant> <other tenant>
isolated() {
  local what=$1 fn=$2 rec=$3 own=$4 other=$5 out deadline=$((SECONDS + TIMEOUT))
  while :; do
    out="$("$fn" "$own" "$rec")"
    [[ "$out" == "200 found" ]] && break
    if ((SECONDS >= deadline)); then
      fail "$what: not found with $HEADER: $own within ${TIMEOUT}s (last: $out)"
      fail "$what: the checks with $HEADER: $other and without the header are skipped (nothing to compare)"
      return
    fi
    sleep 2
  done
  pass "$what: found with $HEADER: $own"
  # Only now does "not found" mean something: the same query found the record a moment ago.
  out="$("$fn" "$other" "$rec")"
  case "$out" in
    "200 empty" | "404 empty") pass "$what: not found with $HEADER: $other ($out)" ;;
    *) fail "$what: with $HEADER: $other got \"$out\" (want not found)" ;;
  esac
  # A header with two tenants ("other|own") must not read the record: the stores have
  # cross-tenant queries off. A refusal (4xx: Mimir 422, Tempo 400; or Loki's 500 "multiple org
  # IDs present", which comes after its query retries, about 17 seconds) or an empty answer
  # passes; "found", no answer (000) or another 5xx fails.
  out="$(CURL_MAX=60 "$fn" "$other|$own" "$rec")"
  case "$out" in
    "200 empty" | "404 empty" | 4[0-9][0-9]" empty" | 5[0-9][0-9]" empty refused") pass "$what: not found with $HEADER: $other|$own ($out)" ;;
    *) fail "$what: with $HEADER: $other|$own got \"$out\" (want refused or not found)" ;;
  esac
  out="$("$fn" "" "$rec")"
  case "$out" in
    "401 "*) pass "$what: query without $HEADER refused ($out)" ;;
    *) fail "$what: query without $HEADER got \"$out\" (want 401)" ;;
  esac
}

# ---- network exposure -----------------------------------------------------------------------

check_exposure() {
  local svc id bindings nets
  for svc in "${SERVICES[@]}"; do
    id="$(docker ps -q --filter "label=com.docker.compose.project=$PROJECT" --filter "label=com.docker.compose.service=$svc" | head -n1)"
    if [[ -z "$id" ]]; then
      fail "$svc: no running container in compose project $PROJECT"
      continue
    fi
    bindings="$(docker inspect -f '{{range $p, $b := .NetworkSettings.Ports}}{{range $b}}{{$p}}={{.HostIp}} {{end}}{{end}}' "$id")"
    if [[ -z "$bindings" ]]; then
      fail "$svc: no published port"
    elif grep -qvE '=127\.0\.0\.1$' <(tr ' ' '\n' <<<"$bindings" | sed '/^$/d'); then
      fail "$svc: a port binds beyond 127.0.0.1: $bindings"
    else
      pass "$svc: every published port binds to 127.0.0.1 ($bindings)"
    fi
    nets="$(docker inspect -f '{{range $n, $v := .NetworkSettings.Networks}}{{$n}} {{end}}' "$id" | xargs)"
    if [[ "$nets" == "${PROJECT}_telemetry" ]]; then
      pass "$svc: on the network ${PROJECT}_telemetry only"
    else
      fail "$svc: networks are \"$nets\" (want ${PROJECT}_telemetry only)"
    fi
  done
}

# ---- run ------------------------------------------------------------------------------------

echo "telemetry check $RUN: OTLP $OTLP, Loki $LOKI, Mimir $MIMIR, Tempo $TEMPO, project $PROJECT"
check_exposure

LOG_A="log-a-$RUN" LOG_B="log-b-$RUN" MET_A="met-a-$RUN" MET_B="met-b-$RUN"
TRACE_A="$(openssl rand -hex 16)" TRACE_B="$(openssl rand -hex 16)" TRACE_N="$(openssl rand -hex 16)"

write_ok logs /v1/logs t-a "$(log_json t-a "$LOG_A")"
write_ok logs /v1/logs t-b "$(log_json t-b "$LOG_B")"
write_ok metrics /v1/metrics t-a "$(metric_json t-a "$MET_A")"
write_ok metrics /v1/metrics t-b "$(metric_json t-b "$MET_B")"
write_ok traces /v1/traces t-a "$(span_json t-a "trace-a-$RUN" "$TRACE_A" "$(openssl rand -hex 8)")"
write_ok traces /v1/traces t-b "$(span_json t-b "trace-b-$RUN" "$TRACE_B" "$(openssl rand -hex 8)")"

write_refused logs /v1/logs "$(log_json none "log-n-$RUN")"
write_refused metrics /v1/metrics "$(metric_json none "met-n-$RUN")"
write_refused traces /v1/traces "$(span_json none "trace-n-$RUN" "$TRACE_N" "$(openssl rand -hex 8)")"

isolated "logs (t-a record)" query_log "$LOG_A" t-a t-b
isolated "logs (t-b record)" query_log "$LOG_B" t-b t-a
isolated "metrics (t-a record)" query_metric "$MET_A" t-a t-b
isolated "metrics (t-b record)" query_metric "$MET_B" t-b t-a
isolated "traces (t-a record)" query_trace "$TRACE_A" t-a t-b
isolated "traces (t-b record)" query_trace "$TRACE_B" t-b t-a

if ((failures > 0)); then
  echo "telemetry check: $failures failure(s)"
  exit 1
fi
echo "telemetry check: all passed"
