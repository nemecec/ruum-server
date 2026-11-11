#!/usr/bin/env bash
# What staging is now (read-only).
#
#   deploy/staging/status.sh            resources by label and their monthly price, nodes, pods,
#                                       backups, deployed commit
#   deploy/staging/status.sh --checks   also the smoke checks (ops/smoke.sh) and Zitadel drift
set -euo pipefail
# shellcheck source=lib.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"
stg_secret_stream "${STG_NAMES_TOFU[@]}" | stg_run ro status "$@"
if [[ "${1:-}" == --checks ]]; then
  bash "$STG_HERE/ops/smoke.sh" || true
fi
