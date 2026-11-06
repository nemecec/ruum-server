#!/usr/bin/env bash
# A curl smoke test of /dav, with the requests that macOS Finder sends (OPTIONS, PROPFIND with
# Depth 0 and 1, the AppleDouble probes, LOCK, an empty PUT and then a chunked PUT with
# X-Expected-Entity-Length, UNLOCK, MOVE, COPY, DELETE).
#
# Usage: DAV_URL=http://127.0.0.1:8597/dav DAV_USER=... DAV_PASS=... scripts/smoke.sh
# DAV_USER is the email of the user and DAV_PASS an app password (ruum_app_...).
set -euo pipefail

: "${DAV_URL:?the /dav URL}" "${DAV_USER:?the email}" "${DAV_PASS:?an app password}"
U=(-sS -u "$DAV_USER:$DAV_PASS" -A "WebDAVFS/3.0.0 (03008000) Darwin/24.0.0 (arm64)")
fail() { echo "FAIL: $*" >&2; exit 1; }
code() { curl "${U[@]}" -o /dev/null -w '%{http_code}' "$@"; }
expect() { local want=$1; shift; local got; got=$(code "$@"); [ "$got" = "$want" ] || fail "$* -> $got, want $want"; echo "ok $want $*"; }

DIR="smoke-$$"
[ "$(curl -sS -o /dev/null -w '%{http_code}' -X PROPFIND "$DAV_URL/")" = 401 ] || fail "no 401 without credentials"
echo "ok 401 without credentials"
curl "${U[@]}" -X OPTIONS -D - -o /dev/null "$DAV_URL/" | tr -d '\r' | grep -Ei '^(dav|ms-author-via|allow):'
expect 207 -X PROPFIND -H 'Depth: 0' "$DAV_URL/"
expect 207 -X PROPFIND -H 'Depth: 1' "$DAV_URL/"
expect 404 -X PROPFIND -H 'Depth: 0' "$DAV_URL/._.DS_Store"
expect 201 -X MKCOL "$DAV_URL/$DIR"
expect 201 -X PUT -H 'Content-Length: 0' --data-binary '' "$DAV_URL/$DIR/note.txt"
TOKEN=$(curl "${U[@]}" -X LOCK -H 'Timeout: Second-600' -H 'Content-Type: text/xml' -D - -o /dev/null \
  --data '<?xml version="1.0"?><D:lockinfo xmlns:D="DAV:"><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype><D:owner><D:href>smoke</D:href></D:owner></D:lockinfo>' \
  "$DAV_URL/$DIR/note.txt" | tr -d '\r' | awk -F': ' 'tolower($1)=="lock-token"{print $2}')
[ -n "$TOKEN" ] || fail "LOCK gave no token"
echo "ok LOCK $TOKEN"
printf 'hello from finder' | curl "${U[@]}" -X PUT -H 'Transfer-Encoding: chunked' \
  -H 'X-Expected-Entity-Length: 17' -H "If: ($TOKEN)" --data-binary @- -o /dev/null -w 'ok %{http_code} chunked PUT\n' "$DAV_URL/$DIR/note.txt"
[ "$(curl "${U[@]}" "$DAV_URL/$DIR/note.txt")" = "hello from finder" ] || fail "GET content"
[ "$(curl "${U[@]}" -H 'Range: bytes=6-9' "$DAV_URL/$DIR/note.txt")" = "from" ] || fail "GET range"
echo "ok GET and Range"
expect 204 -X UNLOCK -H "Lock-Token: $TOKEN" "$DAV_URL/$DIR/note.txt"
expect 201 -X MOVE -H "Destination: $DAV_URL/$DIR/renamed%20note.txt" "$DAV_URL/$DIR/note.txt"
expect 201 -X COPY -H "Destination: $DAV_URL/$DIR-copy" "$DAV_URL/$DIR"
expect 200 "$DAV_URL/$DIR-copy/renamed%20note.txt"
expect 204 -X DELETE "$DAV_URL/$DIR-copy"
expect 204 -X DELETE "$DAV_URL/$DIR"
expect 404 -X PROPFIND -H 'Depth: 0' "$DAV_URL/$DIR"
echo "smoke: all passed"
