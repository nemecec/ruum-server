#!/usr/bin/env bash
# How a staging plan reaches the Talos API (backlog O-21, README "Admin access"). The firewall
# opens the Talos API only to the build server, so the Talos resources of tofu/cluster dial the
# nodes through the admin tunnel: endpoint 127.0.0.1:1500N (tunnel.sh), node by its public IPv4
# (the endpoint's apid relays to itself). This script only reads the JSON it gets (jq); it
# contacts nothing and prints no Talos value but endpoints. steps.sh runs it in the tools
# container; tests/run.sh runs it on fixtures.
#
#   talos-access.sh state-endpoints     stdin: `tofu show -json` of the cluster state (piped, never
#                                       on disk). Prints the endpoints of the Talos resources as a
#                                       JSON map {"stg-N": ..., "bootstrap": ...}; only values of the
#                                       form 127.0.0.1:1500N or an IPv4 address are kept.
#   talos-access.sh touched PLAN        the keys (stg-N, bootstrap) whose Talos resource the plan
#                                       creates, updates or replaces (these contact the node), one
#                                       per line
#   talos-access.sh keep MAP PLAN       MAP (state-endpoints) without the touched keys: the endpoints
#                                       that a second plan keeps (every other one is the tunnel)
#   talos-access.sh kind PLAN           create     the plan creates a server (new addresses: a
#                                                  targeted apply of the servers first, then
#                                                  ops/tunnel-setup.sh, then the rest)
#                                       bootstrap  the plan creates the bootstrap (stg-1's Kubernetes
#                                                  API starts only after it)
#                                       tunnel     the plan changes Talos resources of running nodes
#                                       none       no Talos resource is created or changed
#   talos-access.sh check PHASE PLAN    PHASE servers: the plan touches no Talos resource.
#                                       PHASE full: the plan creates no server, and the admin tunnel
#                                       port of every touched node is open (STG_TUNNEL_PORTS).
# PLAN is the scrubbed plan JSON (plan-scrub.jq keeps the addresses and the actions).
set -euo pipefail

die() { printf 'talos-access: %s\n' "$*" >&2; exit 1; }
command -v jq >/dev/null || die "jq is required"

# The keys of the Talos resources that the plan creates, updates or replaces.
TOUCHED='[ .resource_changes[]? | select(.mode == "managed")
  | select(.type == "talos_machine_configuration_apply" or .type == "talos_machine_bootstrap")
  | select((.change.actions | index("create")) != null or (.change.actions | index("update")) != null)
  | if .type == "talos_machine_bootstrap" then "bootstrap" else (.index | tostring) end ] | unique'

plan_file() {
  [[ -r "${1:-}" ]] || die "no plan JSON '${1:-}'"
  jq -e 'has("prior_state") | not' "$1" >/dev/null || die "the plan JSON holds the prior state; scrub it first (plan-scrub.jq)"
}

touched() {
  plan_file "$1"
  jq -r "$TOUCHED | .[]" "$1"
}

case "${1:-}" in
  state-endpoints)
    jq -cS '[ .values.root_module // {} | recurse(.child_modules[]?) | .resources[]?
      | select(.mode == "managed")
      | if .type == "talos_machine_configuration_apply" then {key: (.index // "" | tostring), value: .values.endpoint}
        elif .type == "talos_machine_bootstrap" then {key: "bootstrap", value: .values.endpoint}
        else empty end
      | select(.key | test("^(stg-[1-4]|bootstrap)$"))
      | select(.value | type == "string" and test("^(127\\.0\\.0\\.1:1500[1-4]|([0-9]{1,3}\\.){3}[0-9]{1,3})$")) ]
      | from_entries'
    ;;
  touched)
    touched "${2:-}"
    ;;
  keep)
    map="${2:-}"
    jq -e 'type == "object" and all(.[]; type == "string")' <<<"$map" >/dev/null 2>&1 || die "keep needs a JSON map of endpoints"
    plan_file "${3:-}"
    jq -cS --argjson m "$map" "($TOUCHED) as \$t | \$m | with_entries(select(.key as \$k | \$t | index(\$k) | not))" "$3"
    ;;
  kind)
    plan_file "${2:-}"
    jq -r "($TOUCHED) as \$t
      | if any(.resource_changes[]?; .mode == \"managed\" and .type == \"hcloud_server\" and ((.change.actions | index(\"create\")) != null)) then \"create\"
        elif any(.resource_changes[]?; .mode == \"managed\" and .type == \"talos_machine_bootstrap\" and ((.change.actions | index(\"create\")) != null)) then \"bootstrap\"
        elif (\$t | length) > 0 then \"tunnel\"
        else \"none\" end" "$2"
    ;;
  check)
    phase="${2:-}"
    plan="${3:-}"
    keys="$(touched "$plan")"
    kind="$(bash "${BASH_SOURCE[0]}" kind "$plan")"
    case "$phase" in
      servers)
        [[ -z "$keys" ]] || die "the servers phase must not create or change a Talos resource: $(echo $keys)"
        ;;
      full)
        [[ "$kind" != create ]] || die "the plan creates servers, whose Talos API the admin tunnel does not reach yet: up.sh --apply applies the servers first (steps.sh apply servers), then runs ops/tunnel-setup.sh (README 'Admin access')"
        # shellcheck source=tunnel.sh
        . "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/tunnel.sh"
        for k in $keys; do
          node="$k"
          [[ "$k" == bootstrap ]] && node=stg-1
          [[ "$node" =~ ^stg-([1-4])$ ]] || die "unexpected Talos resource key '$k'"
          port=$((TUNNEL_TALOS_BASE + BASH_REMATCH[1]))
          [[ " ${STG_TUNNEL_PORTS:-} " == *" $port "* ]] ||
            die "the plan changes the Talos resource of $k, but the admin tunnel port $port is not open (up.sh --apply opens the tunnel first; README 'Admin access')"
        done
        ;;
      *) die "check needs the phase servers or full" ;;
    esac
    ;;
  *)
    die "usage: talos-access.sh state-endpoints | touched PLAN | keep MAP PLAN | kind PLAN | check servers|full PLAN"
    ;;
esac
