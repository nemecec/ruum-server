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
| `GET /images/{ref}/json` | start: is the image local |
| `POST /images/create?fromImage={RUUM_REGISTRY}/ruum/{tenant}/{app}@{digest}` | start: pull |
| `POST /networks/create` | first instance of an app and environment |
| `GET /networks/{name}` | after create (read back `internal`, `inhibit_ipv4`, IPv6, subnet) |
| `POST /networks/{id}/connect` | attach a shared container of a route (with its alias) |
| `POST /networks/{id}/disconnect` | last instance gone |
| `DELETE /networks/{id}` | last instance gone |
| `POST /containers/create?name=ruum-i-{instance}` | start |
| `GET /containers/{name}/json` | after create (read back every flag and limit), health, exit |
| `POST /containers/{name}/attach?stream=1&stdout=1&stderr=1` | before each start (logs) |
| `POST /containers/{name}/start` | start, wake |
| `POST /containers/{name}/wait` | each run (its end) |
| `POST /containers/{name}/stop?t={stop timeout}` | stop, scale to zero |
| `POST /containers/{name}/kill?signal=SIGKILL` | job timeout |
| `DELETE /containers/{name}?force=1&v=1` | stop, end of a job, failed start |

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
```

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
  kernel. The sandbox suite records it as a finding.
- Under runsc, the pids limit bounds the sandbox's host threads (gVisor's own included), not the
  app's processes one by one: below about 64 the sandbox does not start.
- The token file is rewritten in place (the same inode, which the bind mount pins); a read at the
  same moment can see a partial token. Apps read it again on an authentication failure.
- The spec has no `egress.allow`: the driver registers an empty allow list (every host passes the
  allow step; the address rules still apply). A network holds one app and environment; its
  registration carries the deployment of its first instance.
