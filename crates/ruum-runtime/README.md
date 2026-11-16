# ruum-runtime: the Docker runtime driver

Card R-2a. `DockerDriver` implements `ruum_core::runtime::RuntimeDriver` on a Docker engine
through `bollard` 0.21.1 (features `http` and `pipe` only: no TLS). The OCI runtime is plain
`runc` or gVisor (`runsc`). The crate docs (`src/lib.rs`) describe the sandbox.

## Settings

| Variable | What |
|---|---|
| `RUUM_DOCKER_HOST` | The engine: `unix:///path/docker.sock` or `tcp://host:port` (I-4's socket proxy). Required; only the tests default to the local socket. |
| `RUUM_RUNTIME_DRIVER` | `docker` (runc, default) or `runsc`. With `runsc` the driver refuses to start when the engine has no runtime `runsc`. |
| `RUUM_REGISTRY` | The local registry (`host:port`). The driver pulls `RUUM_REGISTRY/ruum/<tenant>/<app>@<digest>` only. |
| `RUUM_RUNTIME_STATE_DIR` | A folder on the engine's host for the workload token files (0700 per instance). |

The crate reads no environment itself: the server's wiring (I-4) reads these settings, adds them
to `crates/ruum-server/env-names.txt` and builds `DockerConfig`. Only the tests read them here.

The shared containers per route (egress proxy, internal gateway, pooler: container, alias,
port) come from the wiring (`DockerConfig::routes`), never from a spec.

## Docker API surface (for I-4's socket proxy)

The driver makes exactly these calls. Every network and container it creates carries the label
`ruum.app`; I-4's socket proxy allows the calls below and only on objects with that label (the
create calls carry the label in their body; `images/create` names only
`RUUM_REGISTRY/ruum/...` references).

| Method and path | When |
|---|---|
| `GET /_ping`, `GET /version` | connect (version negotiation) |
| `GET /info` | connect, for `runsc` only (is the runtime registered) |
| `GET /images/{ref}/json` | start: is the image local; after the pull, its `Config.Volumes` (an image with a `VOLUME` is refused) |
| `POST /images/create?fromImage={RUUM_REGISTRY}/ruum/{tenant}/{app}@{digest}` | start: pull |
| `POST /networks/create` | first instance of an app and environment (a 409 there: the network of an earlier run, adopted when its labels match) |
| `GET /networks/{name}` | after create or on adoption (labels; read back `internal`, `inhibit_ipv4`, IPv6, subnet, the attached containers), after attaching the shared containers and at each start (their addresses) |
| `POST /networks/{id}/connect` | attach a shared container of a route (with its alias), unless it is attached already |
| `POST /networks/{id}/disconnect` | last instance gone; on adoption, a shared container of a route the spec does not have |
| `DELETE /networks/{id}` | last instance gone (after the egress unregistration passed) |
| `POST /containers/create?name=ruum-i-{instance}` | start |
| `GET /containers/{name}/json` | after create (read back every flag, limit and mount), health, exit, an error on the wait stream (does it still run) |
| `POST /containers/{name}/attach?stream=1&stdout=1&stderr=1` | before each start (logs) |
| `POST /containers/{name}/start` | start, wake |
| `POST /containers/{name}/wait` | each run (its end; again after an error while the container runs) |
| `POST /containers/{name}/stop?t={stop timeout}` | stop, scale to zero |
| `POST /containers/{name}/kill?signal=SIGKILL` | job timeout; a refused stop; again each second while a stopped or killed container does not end |
| `DELETE /containers/{name}?force=1&v=1` | stop, end of a job, failed start; 10 s after the first SIGKILL when the container still runs |

Outside the engine: `HEAD {registry}/v2/ruum/{tenant}/{app}/manifests/{digest}` (the repository
check) and, with the egress route, the egress proxy's control listener (`PUT` and `DELETE
/v1/networks/{id}`, `EgressRegistry`).

### Labels

| Label | On | Value |
|---|---|---|
| `ruum.app` | networks, containers | the app node ID |
| `ruum.tenant` | networks, containers | the tenant ID |
| `ruum.env` | networks, containers | `live` or `preview` |
| `ruum.deployment` | containers | the deployment ID |
| `ruum.instance` | containers | the instance ID |
| `ruum.managed` | networks, containers | `ruum-runtime` |

No label holds an environment value. The tests add `ruum.test=s3w3r2a`.

## Tests

On the build server (`scripts/buildbox/run.sh`), one at a time:

```sh
cargo test -p ruum-runtime --test conformance                              # runc
RUUM_RUNTIME_DRIVER=runsc cargo test -p ruum-runtime --test conformance    # gVisor
RUUM_SANDBOX_FULL=1 cargo test -p ruum-runtime --test sandbox -- --nocapture
RUUM_SANDBOX_FULL=1 RUUM_RUNTIME_DRIVER=runsc cargo test -p ruum-runtime --test sandbox -- --nocapture
cargo test -p ruum-runtime --test cold_start -- --nocapture                # prints median and p95
cargo test -p ruum-runtime --test restart                                  # a new driver adopts the network
```

The conformance summary, the sandbox report (target set and every row) and the cold start
numbers go to standard error past the test harness's capture, so the CI log shows them also
when the tests pass. With `CI` set the sandbox suite refuses `RUUM_SANDBOX_CASES`.

The tests build `testdata/fixture` (the conformance program and the sandbox probes on
`node:22-alpine`, pinned by digest), start a `registry:2` (pinned by digest) and stand-in shared
containers, and remove everything labelled `ruum.test=s3w3r2a` at the start and the end. In CI
they run with plain Docker; without an engine they fail when `CI` is set.

## Known limits

- Redaction is exact match only: hex, base64 and other encodings of a secret pass (rule 7's
  known limit; the telemetry pipeline, O-1a, looks for them). The sandbox suite checks that this
  is so, so nobody takes it for more.
- Under runsc, Docker's seccomp profile is not applied inside the sandbox unless runsc runs with
  `--oci-seccomp` (a root setting in `daemon.json`); `unshare -U` then works inside gVisor's own
  kernel. The sandbox suite records it as a finding, and the driver logs a warning at connect
  when the engine's `runsc` has no `--oci-seccomp` in its runtime arguments. Infrastructure
  (I-6) decides on the flag (decision 0006 G point 5 wants user namespaces under runsc for
  Railpack).
- An image that declares a `VOLUME` is refused (`unsupported_image`): Docker would give the path
  an anonymous, writable volume on the engine's disk with no size limit. `verify` also requires
  that the container's mounts are the token bind only.
- Secret values are in the container's `Config.Env`, so `GET /containers/{name}/json` shows
  them. I-4's socket proxy gives that call (and every other) to the server only; whoever can
  call the engine API can also run code in every container, so it is a root boundary either way.
- Redaction bounds: at most 256 values and 256 KiB per spec (`check_secret_set`), 16 KiB per
  value, and the newest 16 workload tokens of an instance (the current one and 15 earlier); an
  older token is no longer redacted. When a value is part of the marker `<hidden secret>`, a
  shorter marker is used, and at last an empty one.
- A server restart: the driver keeps its instances in memory. The next start of an app whose
  network an earlier run left adopts that network (labels `ruum.managed`, `ruum.tenant`,
  `ruum.app`, `ruum.env` must match, and the read-back must pass), attaches the missing shared
  containers, detaches the ones of other routes and registers it at the egress proxy again. The
  old containers keep running unmanaged and their usage is lost until the full reconciliation
  at connect (backlog R2A-1).
- A failed egress unregistration is tried three times; then the network stays (its subnet stays
  taken, so no new network gets an overlapping one that the proxy would refuse) and the driver
  tries again at the next network creation.
- A stop, scale to zero or job kill that does not end the container: SIGKILL again each second,
  and 10 s after the first SIGKILL the container is removed by force. `stop` and `wake` wait for
  the end at most the stop timeout plus 22 s, then answer `unavailable`.
- Under runsc, the pids limit bounds the sandbox's host threads (gVisor's own included), not the
  app's processes one by one: below about 64 the sandbox does not start.
- The token file is rewritten in place (the same inode, which the bind mount pins); a read at the
  same moment can see a partial token. Apps read it again on an authentication failure.
- The sandbox finds the shared containers by `/etc/hosts` (`ExtraHosts`: alias and the address
  the engine gave on the app network), not by Docker's embedded DNS: under runsc the sandbox
  cannot reach 127.0.0.11, so there an app resolves no other name (under runc the embedded DNS
  answers for the containers of the same network only).
- The spec has no `egress.allow`: the driver registers no allow list (`None`: every public host
  passes the allow step; the address rules still apply). In R-2e's API an empty list
  (`Some(vec![])`) would let no host pass. A network holds one app and environment; its
  registration carries the deployment of its first instance.
