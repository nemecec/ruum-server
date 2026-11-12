# Spike S-2: the sandbox, the builder and the gateway

Card S-2 of `docs/STEP3.md`. The spike checks, on the build server, the rules of "How an app runs"
before the runtime driver (R-1, R-2a), the builder (R-2b) and the gateway (G-1a, G-1b) freeze. The
result is in `docs/decisions/0006-step3-spec.md`, section G "Spike S-2 result".

## Run it

On the build server (`scripts/buildbox/README.md`):

```sh
scripts/buildbox/run.sh <name> <worktree> -- spikes/step3-sandbox/run.sh          # every point
scripts/buildbox/run.sh <name> <worktree> -- spikes/step3-sandbox/run.sh 2 4      # some points
scripts/buildbox/run.sh <name> <worktree> -- spikes/step3-sandbox/run.sh --clean  # remove leftovers
```

It prints `PASS`, `FAIL` or `SKIP` per check and exits non-zero on a `FAIL`. It needs Docker,
`cargo`, `curl`, `jq`, `openssl` and `python3` on the host, the dev stack's containers
(`ruum-garage-1`, `ruum-postgres-1` and the others on `ruum_default`), and access to the internet
for the egress checks.

Everything the script makes is named `s3w1s-*`: containers, networks, images, the Garage bucket
`s3w1s-static` and the Garage key `s3w1s-key`. It removes them at the end of every run. The
user-token key and the Garage secret live in `~/s3w1s-work` (mode 700) during a run, and the script
never prints them. Hostile probes run only from the script's own containers, only as TCP connects
and one HTTP GET to the metadata address, and never target staging or the SSH tunnel.

## Files

| File | What |
|---|---|
| `run.sh` | Every check. |
| `gw/` | A throwaway Rust gateway and egress proxy (its own cargo workspace, not a member of the Ruum workspace; every crate at the version of the Ruum `Cargo.lock`). Subcommands `proxy`, `static`, `put`, `clean`, `egress`. |
| `app/` | A small Node app (and its image): it echoes the headers it got and checks `Ruum-User` as the SDK would. |
| `site/` | The static app of point 4. |
| `probe/` | The probe of points 1 and 5: TCP connects, the metadata GET, DNS, the Docker socket. |
| `hostile/` | The hostile `package.json` of point 5. |

## What it checks

1. **Sandbox.** A Node app in plain Docker with the driver's flags (`--read-only`, `--cap-drop ALL`,
   `--security-opt no-new-privileges`, a non-root user, the default seccomp profile, CPU, memory
   and process limits, one `internal: true` network per app, no Docker socket) cannot reach the
   platform Postgres, the data cluster, SpiceDB, OpenBao, NATS, Garage, the metadata address, the
   Docker socket, the host or another app's network, and reaches the internet only through the
   egress proxy (`gw egress`), which refuses private, link-local and platform addresses.
2. **Gateway headers.** The gateway removes every incoming `Ruum-*` header (also `Ruum_User`), the
   gateway cookies, partner keys, `traceparent` and `X-Forwarded-*`, adds a fresh signed
   `Ruum-User` (EdDSA, 60 s), and drops an app's `Set-Cookie` for any `ruum` name. The app checks
   the token as the SDK would; a token for app A fails app B's check.
3. **Cold start.** Median and p95 of 20 starts of a stopped container until the first HTTP answer.
4. **Static files.** A static archive, unpacked into Garage under the deployment's prefix, served
   byte for byte through the gateway; no path reaches another prefix.
5. **Hostile build.** A `package.json` whose `postinstall` calls the metadata address, the platform
   Postgres and the Docker socket runs `npm ci` in a sandbox job with the driver's flags and
   reaches nothing; the packages come only through the egress proxy.

`r` (runsc): gVisor needs a person on the build server (root to register the runtime). The script
uses `runsc` where Docker knows it, and else tries `~/bin/runsc --rootless`; the result says which.
