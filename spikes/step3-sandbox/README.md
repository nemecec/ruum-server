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

It prints `PASS`, `FAIL` or `SKIP` per check and exits non-zero on a `FAIL`. `SKIP` is only for
runsc. It first runs the gateway's unit tests (`cargo test` in `gw/`) and stops if they fail. It needs Docker,
`cargo`, `curl`, `jq`, `openssl` and `python3` on the host, the dev stack's containers
(`ruum-garage-1`, `ruum-postgres-1` and the others on `ruum_default`), and access to the internet
for the egress checks.

Everything the script makes is named `s3w1s-*`: containers, networks, images, the Garage bucket
`s3w1s-static` and the Garage key `s3w1s-key`. It removes them at the end of every run (also when
it fails), together with the images it pulled itself (the Node, Ubuntu and BuildKit images when
they were not on the host before). The
user-token key and the Garage secret live in `~/s3w1s-work` (mode 700) during a run, and the script
never prints them. Hostile probes run only from the script's own containers, only as TCP connects
and one HTTP GET to the metadata address, and never target staging or the SSH tunnel. The positive
controls connect from the host (or a spike container) to the same addresses and close at once.

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
   and process limits, one `internal: true` network per app created with
   `-o com.docker.network.bridge.inhibit_ipv4=true`, no Docker socket) cannot reach the
   platform Postgres, the data cluster, SpiceDB, OpenBao, NATS, Garage, the metadata address, the
   Docker socket, the host or another app's network, and reaches the internet only through the
   egress proxy (`gw egress`), which refuses private, link-local and platform addresses and the
   host's public addresses (`EGRESS_DENY`: addresses or CIDR prefixes, checked on the IPv4 form of
   an IPv4-mapped IPv6 address). It also probes the host over IPv6 (the bridge's link-local address
   and the host's public IPv6 address) and records whether IPv6 is on inside the sandbox. A target
   that has no address (a container that is down) is a `FAIL`, and the probe must answer for
   exactly the expected set of targets.

   **No check passes on silence.** "Not reached" alone proves nothing: a target that is down, or a
   firewall that drops packets, looks the same. So every TCP target, the metadata GET and the DNS
   names have a **positive control** that must reach them at the same time: a connect from the
   host to the same address, a container on the target's own network (another app), or, for the
   host behind the app network's would-be gateway, the probe on a plain internal network (where
   sshd answers). A probe whose control fails is a `FAIL`. A refused egress request passes only on
   the proxy's own refusal: status 403 (405 for plain HTTP) with the header `X-Ruum-Egress-Deny`
   and the expected reason (`port`, `allow-list`, `address`, `method`). A 502, a timeout, a TLS
   error after a 200 or a 403 with another reason is a `FAIL`. The refused list includes
   addresses that the allow-list lets through (numeric, decimal `2852039166`, octal, hex, `127.1`,
   IPv4-mapped IPv6, `::1`, `fd00:ec2::254`, the host's IPv6 address and another address in its
   /64), so only the address rules can refuse them. Host ports that listen on 127.0.0.1 only
   (the dev stack's published ports) are not targets: no control reaches them on another address.
   The host-bridge check first asserts that the network and its bridge device exist and that its
   IPv6 setting is readable; an empty answer from a missing device is a `FAIL`.
2. **Gateway headers.** The gateway removes every incoming `Ruum-*` header (also `Ruum_User`), the
   gateway cookies, partner keys, `traceparent`, `tracestate`, `baggage`, `Forwarded`, `X-Real-IP`
   and every `X-Forwarded-*` name, adds a fresh signed `Ruum-User` (EdDSA, 60 s), and drops an
   app's `Set-Cookie` for any `ruum` name, with an empty name (`=ruum_app=x`, which a browser sends
   back as `ruum_app=x`) or without `=`. It adds no `X-Forwarded-For`: apps do not get the
   visitor's IP address. The app checks the token as the SDK would; a token for app A fails app B's
   check, and so do tokens with a wrong issuer, type, environment or algorithm, without `iat` or
   `exp`, expired, or living longer than 60 s.
3. **Cold start.** Median and p95 of 20 starts of a stopped container until the first HTTP answer.
4. **Static files.** A static archive, unpacked into Garage under the deployment's prefix, served
   byte for byte through the gateway; no path reaches another prefix. The object outside the
   prefix is read back first (so it exists), every traversal path must get the gateway's own 404
   or 400 (not `000` or a 5xx), and the gateway must still answer afterwards.
5. **Hostile build.** A `package.json` whose `postinstall` calls the metadata address, the platform
   Postgres and the Docker socket runs `npm ci` in a sandbox job with the driver's flags and
   reaches nothing; the packages come only through the build's own egress proxy (a second proxy
   container, so point 1's log cannot answer this check). The job's output folder `/out` is a
   Docker volume on tmpfs with a 64 MiB limit, not a host folder; the postinstall tries to write
   96 MiB there and must stop with `ENOSPC`. The output is copied out with a byte cap, and only
   the expected regular files are kept. The built layer (a plain tar) runs in a sandbox from a
   read-only folder. The spike does not assemble an OCI image from the layer (R-2b does). The script also tries rootless BuildKit under the same flags and prints the
   outcome as an `INFO` line (it does not start: no user namespaces).

**For the production recipe (R-2b):** never give a build job a writable host bind mount. Its
output goes to a size-limited sandbox filesystem (a tmpfs volume with a size, or a quota) and
leaves through a byte-capped, checked copy or stream. The base image is pinned by digest, as the
spike does (`node:22-alpine@sha256:...`, `ubuntu:24.04@sha256:...`).

Not tested by the spike: UDP (all direct probes are TCP; a DNS lookup that fails is not a UDP
test), IPv6 egress (the outside network has no IPv6), a DNS rebinding answer (the `nip.io` names
are static), Node's own `fetch` through `HTTPS_PROXY`, and OCI image assembly.

Why `inhibit_ipv4`: without it the host has an address on every bridge, also on an internal one,
and an app reaches every host service that listens on all addresses (on the build server: `sshd`)
through its network's gateway. Point 1 shows this for a plain internal network as an `INFO` line.

## runsc

`r` runs point 1 again under gVisor when Docker knows the `runsc` runtime (and point 3 then measures
runsc too); else it tries `~/bin/runsc --rootless`; else it reports `SKIP`. Registering runsc needs
root on the build server, so a person runs (from the gVisor install guide):

```sh
# as root on the build server
ARCH=$(uname -m)
URL=https://storage.googleapis.com/gvisor/releases/release/latest/${ARCH}
cd "$(mktemp -d)"
wget ${URL}/runsc ${URL}/runsc.sha512 ${URL}/containerd-shim-runsc-v1 ${URL}/containerd-shim-runsc-v1.sha512
sha512sum -c runsc.sha512 -c containerd-shim-runsc-v1.sha512
chmod a+rx runsc containerd-shim-runsc-v1
mv runsc containerd-shim-runsc-v1 /usr/local/bin
/usr/local/bin/runsc install   # adds the runsc runtime to /etc/docker/daemon.json
systemctl reload docker        # a reload is enough for a new runtime; the dev stack keeps running
docker run --rm --runtime=runsc hello-world
```

Then run `spikes/step3-sandbox/run.sh 1 3 r` again.
