#!/usr/bin/env bash
# Sets up or moves the ruum-ops user on the build server (review 2 O-1, review 1 S-2, code
# review F-2). Only the person runs it, as root, from a copy that they read first:
#
#   sudo bash install-ops-user.sh <commit> <ops-public-key-file>
#
# What it does:
# - ruum-ops: a system user with home /home/ruum-ops (0700). The kubeconfig, talosconfig and
#   plans live in ~ruum-ops/.ruum-staging (0700). Build jobs run as `ruum` and cannot read them.
# - /opt/ruum-staging/<commit>: a root-owned checkout of a commit of main. `current` points at
#   it. ruum-ops runs the staging scripts only from there, never from a writable checkout.
# - The commit must be on main (an ancestor of origin/main); a branch commit is refused. When
#   /etc/ruum-staging/allowed_signers exists (root-owned, the person's SSH signing keys), the
#   commit must also carry a good signature by one of them (git verify-commit). Without that
#   file, the script shows the diff of deploy/staging since the current commit and asks the
#   person to type the new commit's first 12 characters.
# - ~ruum-ops/.ssh/authorized_keys (root-owned): the laptop's ops key, with a forced command
#   (ops/ssh-entry.sh of the pinned checkout) and `restrict` (no pty, no forwarding). The laptop
#   scripts reach ruum-ops over ssh with that key (lib.sh, mode "ops").
# - It writes no sudoers rule. `ruum` gets no way to run anything as ruum-ops or root through
#   this setup. The person's decision on the existing sudo rules of `ruum` is in
#   panel/staging-buildbox-hardening.md; this script only reports them.
# - The tools (setup.sh, pinned by SHA-256) are installed into ~ruum-ops/.local/bin.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "run with sudo, as the person" >&2; exit 1; }
commit="${1:?usage: install-ops-user.sh <commit> <ops-public-key-file>}"
pubkey_file="${2:?usage: install-ops-user.sh <commit> <ops-public-key-file>}"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "give the full 40-character commit" >&2; exit 1; }
REPO=https://github.com/nemecec/ruum-server.git
BASE=/opt/ruum-staging
SIGNERS=/etc/ruum-staging/allowed_signers

# The ops key: exactly one ssh-ed25519 public key line, no options of its own.
pubkey="$(tr -d '\r' <"$pubkey_file")"
[[ "$(wc -l <<<"$pubkey")" -eq 1 && "$pubkey" =~ ^ssh-ed25519\ [A-Za-z0-9+/=]+(\ [A-Za-z0-9@._-]+)?$ ]] ||
  { echo "$pubkey_file must hold one ssh-ed25519 public key line" >&2; exit 1; }

mkdir -p "$BASE"
chown root:root "$BASE"
chmod 755 "$BASE"
[[ -d "$BASE/repo/.git" ]] || git clone --quiet --no-checkout "$REPO" "$BASE/repo"
# Only main and the tags: branch commits are never fetched into this clone.
git -C "$BASE/repo" fetch --quiet --prune origin '+refs/heads/main:refs/remotes/origin/main' '+refs/tags/*:refs/tags/*'
git -C "$BASE/repo" cat-file -e "$commit^{commit}" 2>/dev/null || { echo "commit $commit is not on main at GitHub" >&2; exit 1; }
git -C "$BASE/repo" merge-base --is-ancestor "$commit" origin/main || { echo "commit $commit is not an ancestor of origin/main" >&2; exit 1; }

previous=""
[[ -L "$BASE/current" ]] && previous="$(basename "$(readlink "$BASE/current")")"
if [[ -f "$SIGNERS" ]]; then
  [[ "$(stat -c %U "$SIGNERS")" == root ]] && [[ "$(stat -c %a "$SIGNERS")" =~ ^[0-7][0-5][0-5]$ ]] ||
    { echo "$SIGNERS must be owned by root and not writable by others" >&2; exit 1; }
  git -C "$BASE/repo" -c gpg.format=ssh -c gpg.ssh.allowedSignersFile="$SIGNERS" verify-commit "$commit" ||
    { echo "commit $commit has no good signature by a key in $SIGNERS" >&2; exit 1; }
  echo "signature of $commit: good ($SIGNERS)"
else
  echo "No $SIGNERS: review the change of deploy/staging before it runs as ruum-ops."
  if [[ -n "$previous" && "$previous" =~ ^[0-9a-f]{40}$ ]]; then
    git -C "$BASE/repo" --no-pager diff --stat "$previous" "$commit" -- deploy/staging
    echo "Full diff: git -C $BASE/repo diff $previous $commit -- deploy/staging"
  else
    git -C "$BASE/repo" --no-pager show --stat --format='%H %s' "$commit" -- deploy/staging | head -40
  fi
  printf 'Type the first 12 characters of %s to install it: ' "$commit"
  read -r answer </dev/tty
  [[ "$answer" == "${commit:0:12}" ]] || { echo "not confirmed; nothing changed" >&2; exit 1; }
fi

id ruum-ops >/dev/null 2>&1 || useradd --system --create-home --home-dir /home/ruum-ops --shell /bin/bash ruum-ops
chown ruum-ops:ruum-ops /home/ruum-ops
chmod 700 /home/ruum-ops

if [[ ! -d "$BASE/$commit" ]]; then
  git -C "$BASE/repo" worktree add --detach "$BASE/$commit" "$commit" >/dev/null
fi
chown -R root:root "$BASE"
chmod -R go-w "$BASE"
ln -sfn "$BASE/$commit" "$BASE/current"

# The laptop's ops key, root-owned, with the forced command of the pinned checkout.
install -d -o root -g root -m 755 /home/ruum-ops/.ssh
printf 'restrict,command="/bin/bash %s/current/deploy/staging/ops/ssh-entry.sh" %s\n' "$BASE" "$pubkey" >/home/ruum-ops/.ssh/authorized_keys.new
chown root:root /home/ruum-ops/.ssh/authorized_keys.new
chmod 644 /home/ruum-ops/.ssh/authorized_keys.new
mv /home/ruum-ops/.ssh/authorized_keys.new /home/ruum-ops/.ssh/authorized_keys

sudo -u ruum-ops -H bash "$BASE/current/deploy/staging/setup.sh" >/dev/null
echo "ruum-ops is ready at $commit (laptop: RUUM_STAGING_REMOTE=ops, key in RUUM_OPS_KEY)"

# Report, never change, the sudo rules that would let a build job reach ruum-ops or root.
if sudo -l -U ruum 2>/dev/null | grep -qE 'NOPASSWD: *ALL|\(ALL( : ALL)?\) *ALL|ruum-ops'; then
  echo "WARNING: user ruum still has sudo rules that reach root or ruum-ops; the separation protects nothing until the person removes them (panel/staging-buildbox-hardening.md)." >&2
fi
if [[ -e /etc/sudoers.d/ruum-staging ]]; then
  echo "WARNING: /etc/sudoers.d/ruum-staging exists (an older version of this script wrote it); see panel/staging-buildbox-hardening.md." >&2
fi
