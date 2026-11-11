#!/usr/bin/env bash
# Generates the staging secrets that nobody has to type, once, into the secrets file (on the Mac).
# A name that is present is never changed. Values are never printed; the script prints names.
#
# Why in the secrets file and not only in the cluster (review 1 S-6): a restore after the loss of
# the whole cluster needs the Zitadel master key, the database passwords and the token key.
# The person keeps a copy of the file (or at least TOFU_STATE_PASSPHRASE) in the password
# manager (review 2 D-4): without the passphrase the state cannot be read and destroy.sh must
# fall back to --sweep.
set -euo pipefail
FILE="${RUUM_SECRETS_FILE:-$HOME/.config/ruum/secrets.env}"
[[ -f "$FILE" ]] || { echo "secrets-init: $FILE does not exist" >&2; exit 1; }
chmod 600 "$FILE"

# A random value of N bytes, base64url without padding, or alphanumeric of length N.
rand_b64url() { openssl rand -base64 "$1" | tr -d '\n=' | tr '+/' '-_'; }
rand_alnum() { LC_ALL=C tr -dc 'A-Za-z0-9' </dev/urandom | head -c "$1"; }

add() {
  local name="$1" value="$2"
  if grep -qE "^(export +)?$name=." "$FILE"; then
    return
  fi
  # Start on a new line even when the file does not end with one.
  [[ -s "$FILE" && "$(tail -c 1 "$FILE")" != "" ]] && printf '\n' >>"$FILE"
  printf '%s=%s\n' "$name" "$value" >>"$FILE"
  echo "secrets-init: added $name"
}

add TOFU_STATE_PASSPHRASE "$(rand_alnum 48)"
# Zitadel's master key: exactly 32 printable bytes.
add STG_ZITADEL_MASTERKEY "$(rand_alnum 32)"
add STG_PG_RUUM_PASSWORD "$(rand_alnum 40)"
add STG_PG_ZITADEL_PASSWORD "$(rand_alnum 40)"
add STG_PG_SPICEDB_PASSWORD "$(rand_alnum 40)"
add STG_SPICEDB_PRESHARED_KEY "$(rand_alnum 48)"
# The Ed25519 seed of Ruum's token key: 32 bytes, base64url.
add STG_RUUM_TOKEN_SIGNING_KEY "$(rand_b64url 32)"
# The key of signed content URLs: base64 of 32 bytes, the same on every server replica.
add STG_RUUM_CONTENT_SIGNING_KEY "$(openssl rand -base64 32 | tr -d '\n')"
# rclone crypt of the blob copy at Scaleway (review 1 S-5).
add STG_RCLONE_CRYPT_PASSWORD "$(rand_alnum 48)"
add STG_RCLONE_CRYPT_SALT "$(rand_alnum 48)"
add STG_NATS_RUUM_PASSWORD "$(rand_alnum 40)"
echo "secrets-init: done (names only above; keep a copy of TOFU_STATE_PASSPHRASE in the password manager)"
