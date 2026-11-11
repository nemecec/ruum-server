#!/usr/bin/env bash
# The forced command of the ruum-ops ssh key (code review F-2). install-ops-user.sh writes
#   restrict,command="/bin/bash /opt/ruum-staging/current/deploy/staging/ops/ssh-entry.sh" <laptop key>
# into ~ruum-ops/.ssh/authorized_keys (root-owned). Whatever command the client asks for arrives
# in SSH_ORIGINAL_COMMAND; this script never evaluates it. It splits it on spaces, accepts only
# plain words (letters, digits and @ : . _ / = + -), only a known remote.sh step, and then runs
# remote.sh of this pinned checkout with those words as arguments. stdin (the secrets) passes on.
#
#   ssh-entry.sh            run the step
#   ssh-entry.sh --parse    print the accepted words, one per line, and run nothing (tests/run.sh)
set -euo pipefail
STG="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STEPS=" plan apply talosconfig platform zitadel-bootstrap check-backup deploy status destroy-plan destroy sweep "
refuse() { echo "ssh-entry: refused: $*" >&2; exit 2; }

cmd="${SSH_ORIGINAL_COMMAND:-}"
[[ -n "$cmd" ]] || refuse "no command (this key runs remote.sh steps only)"
[[ ${#cmd} -le 1024 ]] || refuse "command too long"
[[ "$cmd" =~ ^[A-Za-z0-9@:._/=+\ -]+$ ]] || refuse "only plain words are accepted"
read -r -a words <<<"$cmd"
[[ ${#words[@]} -ge 1 && ${#words[@]} -le 12 ]] || refuse "1 to 12 words"
[[ "$STEPS" == *" ${words[0]} "* ]] || refuse "unknown step ${words[0]}"

if [[ "${1:-}" == --parse ]]; then
  printf '%s\n' "${words[@]}"
  exit 0
fi
exec bash "$STG/remote.sh" "${words[@]}"
