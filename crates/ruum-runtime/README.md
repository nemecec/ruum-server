# ruum-runtime: the Docker and Kubernetes runtime drivers

Card R-2a (Docker) and backlog I6-1 (Kubernetes, below). `DockerDriver` implements `ruum_core::runtime::RuntimeDriver` on a Docker engine
through `bollard` 0.21.1 (features `http` and `pipe` only: no TLS). The OCI runtime is plain
`runc` or gVisor (`runsc`). The crate docs (`src/lib.rs`) describe the sandbox.

## Settings

| Variable | What |
|---|---|
| `RUUM_DOCKER_HOST` | The engine: `unix:///path/docker.sock` or `tcp://host:port` (I-4's socket proxy). Required; only the tests default to the local socket. |
| `RUUM_RUNTIME_DRIVER` | `docker` (runc, default) or `runsc`. With `runsc` the driver refuses to start when the engine has no runtime `runsc`. |
| `RUUM_REGISTRY` | The local registry (`host:port`). The driver pulls `RUUM_REGISTRY/ruum/<tenant>/<app>@<digest>` only. |
| `RUUM_RUNTIME_STATE_DIR` | A folder on the engine's host for the workload token files (0700 per instance). |

`DockerConfig::supervisor` (no variable yet; I-4 wires the Docker driver): the static
`ruum-supervisor` binary on the engine's host (backlog I6-20, "The sandbox supervisor" below).
With it the driver binds it read-only at `/ruum/bin/ruum-supervisor` as the entrypoint and gives
the secret values on standard input; without it the app runs directly with its secret values in
its environment. `max_live_log_bytes` and `max_tenant_live_log_bytes` (64 MiB, 16 MiB) bound the
live instances' log rings (backlog S-91).

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
| `POST /containers/{name}/attach?stream=1&stdout=1&stderr=1` (and `stdin=1` with the supervisor) | before each start (logs; with the supervisor the launch data goes in on standard input right after the start, then standard input is closed) |
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
- Without the supervisor, secret values are in the container's `Config.Env`, so `GET
  /containers/{name}/json` shows them. With it (`DockerConfig::supervisor`), they go in once on
  standard input (`OpenStdin`, `StdinOnce`) and `verify` checks that `Config.Env` holds none. I-4's
  socket proxy gives the engine API to the server only; whoever can call it can also run code in
  every container, so it is a root boundary either way.
- The live instances' log rings count to `max_live_log_bytes` and each tenant's to
  `max_tenant_live_log_bytes` (backlog S-91, `logs::LogBudget`): a ring whose tenant is past its
  share drops its own oldest lines as lines arrive (never past the budget plus one share), and
  each sweep takes from the tenant with the most bytes until the budget holds. An ended
  instance's ring leaves the budget and counts to `max_ended_log_bytes`.
- Redaction bounds: at most 256 values and 256 KiB per spec (`check_secret_set`), 16 KiB per
  value, and the newest 16 workload tokens of an instance (the current one and 15 earlier); an
  older token is no longer redacted. When a value is part of the marker `<hidden secret>`, a
  shorter marker is used, and at last an empty one.
- Ended instances (stopped or exited, the sandbox freed) are forgotten with their log rings
  (backlog R2A-5): after `ended_retention` (15 min), or `drained_retention` (60 s) after their
  logs were read to the end; at once when more than `max_ended_instances` (1024) are kept or
  their logs take more than `max_ended_log_bytes` (256 MiB). Under a cap the driver forgets the
  oldest ended instance of the tenant with the largest share (the most ended log bytes for the
  byte cap, else the most ended instances), so one tenant's load forgets its own ended instances
  first, not another tenant's (S-61). A forgotten instance is `instance_not_found`. At most
  `max_instances` (4096) instances are kept, running and ended together: a start beyond it
  first forgets the oldest ended instance of the tenant that keeps the most instances (running
  and ended, the new one counted for the starting tenant), or else one of the starting tenant's
  own; otherwise it is `unavailable`. So a tenant that fills the driver with running instances
  does not forget another tenant's ended instance (Kimi review of run 12, S-67). That cap is
  global; a per-tenant share of it is backlog R2A-9. The sweep runs at each start and
  each end of an instance, so nothing runs while the driver is idle; the caps hold anyway.
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

## Kubernetes driver

Backlog I6-1. `KubeDriver` (`src/kube/`) implements the same trait on Kubernetes, for staging
(Talos has no Docker engine): one pod per run in the namespace of the sandboxes (`ruum-apps`),
of the runtime class `gvisor`, with the sandbox supervisor as its first process (backlog I6-20,
S-89; "The sandbox supervisor" below and `docs/STEP3.md`). The same spec, limits, routes, token
file and log redaction as the Docker driver; it passes the conformance suite unchanged against a
fake API server (`tests/kube.rs`). No Kubernetes client crate: plain JSON over `reqwest`.

### Objects

Per instance `<n>` = `ruum-i-<instance id without dashes>`:

| Object | What |
|---|---|
| ConfigMap `<n>` | The record (key `record`): identity, kind, routes, limits, image, command, plain environment, state, run, touches, the egress registration, and `sealed` (backlog S-89, `src/kube/seal.rs`): AES-256-GCM under the record key (`v1.<key id>.<generation>.<nonce>.<ciphertext>`, a fresh random nonce per seal, associated data = record name, record `uid`, generation) of the secret values, the previous values, the workload token, the app's command, the digest of the fixed fields, the run and the `uid` of its pod (sealed after the pod exists), and the pods that took their launch data. Every state change replaces it with its `resourceVersion`, so of two server processes one makes the change and its effects (usage, unregistration, pod deletion). Any process answers for any instance and can unseal it: adoption after a restart (R2A-1) and a wake on another replica are the normal path. |
| Pod `<n>-<run>` | One per run; a cold start makes the next run. Labels as the Docker driver's plus `ruum.deployment`, `ruum.instance`, `ruum.run`. |

No Secret: the runtime identity has no verb on Secrets, and the namespace admits none.

The record is shared by every server process, also by two releases during a rolling update
(design 12.7, decision 42). A release keeps the fields it does not know when it writes a record
back (`Record::extra`). A new field, state or kind is expand, then contract: the older release must
read it, or leave the record alone, never rewrite it into something it does not mean.

### The pod

`runtimeClassName` (the setting; `gvisor` on staging), the ServiceAccount `ruum-sandbox` (no
rights), `automountServiceAccountToken: false`, `enableServiceLinks: false`, no host network, PID
or IPC namespace, `restartPolicy: Never`, `dnsPolicy: None` (nameserver `127.0.0.1`: no DNS; the
routes' shared services are `hostAliases` with the address the driver resolved in its own pod),
pod and container user `10001`, `runAsNonRoot`, `fsGroup` 10001, `RuntimeDefault` seccomp.

- The init container `ruum-supervisor` (the pinned `supervisor_image`) runs
  `/ruum-supervisor install /ruum/bin/ruum-supervisor` into the memory volume `ruum-bin`.
- The container `app`: the image by digest from the repository of the spec's tenant and app
  (checked at the registry first, and its config read for the command), `command`
  `/ruum/bin/ruum-supervisor run --launch-url http://<listener ip>:<port> --identity-file
  /ruum/id/token --token-file /var/run/ruum/token` (no `args`: the app's command is in the sealed
  launch data), `readOnlyRootFilesystem`, `allowPrivilegeEscalation: false`, `privileged: false`,
  `capabilities.drop: [ALL]`, CPU and memory limits with requests equal to them, the plain
  environment only, the termination message path `/dev/termination-log` with the policy `File`.
- Volumes: `tmp` (`emptyDir`, `medium: Memory`, `sizeLimit` = the disk limit; a full disk is
  `ENOSPC`) at `/tmp`; `run` (memory, 1 MiB) at `/var/run/ruum`, where the supervisor writes the
  token file; `ruum-bin` (memory, 16 MiB) **read-only** at `/ruum/bin`; `ruum-id`, one projected
  service account token of the audience `ruum-supervisor`, `expirationSeconds: 600`, read-only at
  `/ruum/id`.
- A service gets its port and, with a health check, an **exec** readiness probe (the configured
  command, busybox `wget -Y off` by default, on `http://127.0.0.1:<port><path>`): inside the
  sandbox, never from the node's network (decision 0006 G point 1). A job gets
  `activeDeadlineSeconds` (its timeout, rounded up to a second), and the driver ends it too when
  the API server did not 5 s later.

After create, and again before the supervisor listener gives a pod anything, the driver checks
the stored pod against the pod it makes for the record (`pod::verify`): exhaustively, every key
of the stored spec must be one the driver sent or one of a fixed list of API server defaults
(`schedulerName`, `nodeName`, the copied `serviceAccount`, `priority: 0`, `preemptionPolicy`,
the not-ready and unreachable tolerations, a port's `protocol: TCP`), every value the driver sent
must be equal (quantities by value), the labels exactly, no annotation (gVisor reads pod
annotations). A difference deletes the pod and fails the start, or refuses the launch. The
namespace's admission policy (`k8s/manifests/apps-admission.yaml`, backlog S-83 and S-89) holds
the shape for every pod, whoever makes it, and admits no Secret in `ruum-apps`.

### States

The record's state moves as the pod shows: a pod that runs is `running` (with the egress route,
only after the pod's address is registered at the egress proxy as `<ip>/32`, key = the pod name);
a terminated container is `exited` (`OOMKilled` is out of memory, the pod reason
`DeadlineExceeded` is timed out, otherwise the exit code); a pod that is gone or cannot start
(`CreateContainerConfigError`, an invalid image name) is `exited` as killed. A service past its
idle timeout (the newest touch of the record and of this process; `touch` writes the record at
most every quarter of the idle timeout) scales to zero: the pod goes, the record (with its seal)
stays, and `wake` makes the next run from the record and seals its new pod's `uid`. Health: healthy when the container is ready;
unhealthy when it is not ready after the start period plus the probe's period times its threshold.
The sweep (every `sweep_every`, 5 s by default, in each process) observes every instance of the
driver's labels, retries an unregistration that failed, deletes pods without a running record (an
exited pod after 60 s, so its log is read), registers running pods at the egress proxy again every
`reregister_every` (a proxy restart forgets them), brings the live log budget back under its
bound, and deletes ended records after `ended_retention`.

The record names a registration (`registered`) before the driver makes it, so a process that
ends the run meanwhile always unregisters it (an unregistration is idempotent). After each
registration (the first one and the sweep's again), the registering process reads the record
again: when the run ended meanwhile, it unregisters, and when that fails it puts the name back on
the ended record for the sweep. A pod being deleted (`deletionTimestamp`) is never running for
the driver: no address for the gateway, no registration. The gateway's address is kept per run:
an observation of another run or state forgets it, and `wake` of a live instance answers from an
observation, not from the bare record.

The egress unregistration comes before the pod's deletion. When it fails (three tries), the pod
stays and the record keeps `registered`, so the address is not given to another pod while the
proxy still names this app for it; the sweep tries again. A wake waits until the previous run's
registration is gone (`runtime_unavailable` until then), and the sweep unregisters a left-over
pod before it deletes it. **This does not fail closed for a pod that ended by itself** (an exit,
an eviction, a lost node): its address is free as soon as the pod is, and until the sweep
unregisters it (up to `sweep_every`, longer while the proxy cannot be reached), a new pod that
gets the address runs as the old app at the egress proxy, whatever tenant it belongs to (the new
pod's own registration gets `subnet_overlap` and it stays `starting`, but its process runs).
Backlog I6-17 keeps `RUUM_KUBE_EGRESS_CONTROL` off until this window is closed, and the server
refuses the setting until backlog S-86 (a registration that outlives its pod with no record of
it) is fixed (Kimi run 15, I6-1 finding 3).

### Logs

The process that started (or woke) an instance follows the pod log (`follow=true`, container
`app`) into a ring per instance, through the redactor of `ruum-core`, with the values it knows
(the spec's secrets, previous values and tokens; a rotation adds the new token before it seals it
into the record). Another process (another replica, or this one after a restart) cannot redact, so its
`logs` and its `rotate_token` are `runtime_unavailable` (a rotation there would put a token in the
pod that the following process does not redact; the token renewal tries again). With `split_log_streams` the two streams are read apart
(`stream=Stdout|Stderr`, which needs the API server's feature gate `PodLogsQuerySplitStreams`);
otherwise both come as one, reported as standard output. A state change to exited waits up to 3 s
for the followers to read the end. A broken stream of a running pod reconnects from that moment
(`tailLines=0`): lines of the gap are lost, none is read twice.

### API calls (the Role of `deploy/staging/charts/platform/templates/runtime-rbac.yaml`)

| Method and path (namespace of the setting) | When |
|---|---|
| `POST /api/v1/namespaces/{ns}/configmaps` | start (the record first) |
| `GET /api/v1/namespaces/{ns}/configmaps/{n}` | status, stop, touch, wake, rotate_token, logs of another process |
| `PUT /api/v1/namespaces/{ns}/configmaps/{n}` | every state change, with `resourceVersion` |
| `GET /api/v1/namespaces/{ns}/configmaps?labelSelector=ruum.managed=ruum-runtime,...` | the sweep |
| `DELETE /api/v1/namespaces/{ns}/configmaps/{n}` (`propagationPolicy: Background`) | the record: after the retention, a failed start |
| `POST /api/v1/namespaces/{ns}/pods` | start, wake |
| `GET /api/v1/namespaces/{ns}/pods/{n}-{run}` | status, the sweep, the log followers, each supervisor request |
| `POST /apis/authentication.k8s.io/v1/tokenreviews` (ClusterRole) | each supervisor request (a positive answer is kept at most 60 s and never past the token's `exp`) |
| `GET /api/v1/namespaces/{ns}/pods?labelSelector=...` | the sweep (pods without a running record) |
| `DELETE /api/v1/namespaces/{ns}/pods/{n}-{run}` (grace = the stop timeout) | stop, scale to zero, the sweep, a refused read-back |
| `GET /api/v1/namespaces/{ns}/pods/{n}-{run}/log?container=app&follow=true` | the log followers |

No verb on Secrets. The fake API server of the tests reads the Role and the ClusterRole from the
chart and refuses every other verb, resource or namespace, so a driver change that needs a new
right fails the tests until the chart has it; it also runs the admission rules, issues and
reviews the projected tokens, and plays the supervisor in each pod.
Outside the API: `HEAD` and `GET {registry}/v2/ruum/{tenant}/{app}/manifests/{digest}` (and the
platform's manifest of an index) and `GET .../blobs/{config digest}` (each object checked against
its digest), and the egress proxy's control listener (`EgressRegistry`, as the Docker driver).

### Settings (read by `ruum-server`, `crates/ruum-server/src/config.rs`)

| Variable | What |
|---|---|
| `RUUM_RUNTIME_DRIVER` | `kubernetes` (the server's default is `memory`). |
| `RUUM_KUBE_API_URL`, `RUUM_KUBE_TOKEN_FILE`, `RUUM_KUBE_CA_FILE` | The API server (`https://` only), a projected token (read for every call), its CA. |
| `RUUM_KUBE_NAMESPACE`, `RUUM_KUBE_RUNTIME_CLASS` | `ruum-apps`, `gvisor` by default. |
| `RUUM_REGISTRY` | The registry the nodes pull from; unset: every app image is `image_not_found`. |
| `RUUM_KUBE_EGRESS_PROXY` | The egress proxy's Service, `host:port` (the route `egress_proxy`, alias `ruum-egress`). |
| `RUUM_KUBE_EGRESS_CONTROL`, `RUUM_KUBE_EGRESS_CONTROL_TOKEN_FILE` | The proxy's control listener (a Unix socket, `unix:/path`; backlog I6-2, I6-17). **Refused** until backlog S-86 is fixed; unset: no registration, the proxy refuses every sandbox. |
| `RUUM_KUBE_SPLIT_LOG_STREAMS` | `on` or `off` (default). |
| `RUUM_KUBE_RECORD_KEY_FILE` | The record key, 64 hexadecimal characters (backlog S-89). Required. |
| `RUUM_KUBE_SUPERVISOR_IMAGE` | The supervisor's image, by digest. Required. |
| `RUUM_KUBE_SUPERVISOR_SERVICE` | The supervisor listener as the sandboxes reach it (`host:port`; the driver resolves the host). Required. |
| `RUUM_KUBE_SUPERVISOR_LISTEN` | The listener of this process (`addr:port`; `crates/ruum-server` `supervisor.rs`). Required. |
| `RUUM_KUBE_SUPERVISOR_NETWORK` | Must be `encrypted`: node-to-node traffic is encrypted and authenticated (the launch channel is plain HTTP; decision 0006 I point 4). |
| `RUUM_KUBE_NODE_ARCH` | The nodes' architecture for a multi-arch image (`amd64` by default). |

### Tests

```sh
cargo test -p ruum-runtime --test kube    # the conformance suite and the driver's rules, fake API server
cargo test -p ruum-runtime --lib kube     # quantities, names
```

No cluster, no Docker: they run in every workspace run and in CI. They do not prove gVisor, the
network policies or the kubelet's behaviour: on staging `ops/gvisor-check.sh`,
`status.sh --netpol` and `ops/rbac-check.sh live` do.

### Known limits of the Kubernetes driver

- No pids limit per pod (Kubernetes sets it per node, `podPidsLimit` of the kubelet; staging has
  none yet: backlog I6-19).
- Each process keeps the logs of at most `max_instances` instances (4096), of at most
  `max_tenant_instances` (512) running or scaled-to-zero instances of one tenant (a start past it
  is `runtime_unavailable`, so one tenant never takes every slot; Kimi run 15 I6-1 finding 5,
  backlog R2A-9 for this driver), and of at most `max_ended_instances` ended ones (1024).
  The running instances' rings count to `max_live_log_bytes` (64 MiB) and each tenant's to
  `max_tenant_live_log_bytes` (16 MiB): a ring past its tenant's share drops its own oldest lines
  as lines arrive (never past the budget plus one share), and each sweep takes from the tenant
  with the most bytes until the budget holds (backlog S-91).
  `max_ended_log_bytes` (128 MiB) bounds the logs of ended and scaled-to-zero instances together:
  past it the oldest of the tenant with the most bytes goes first (an ended instance is forgotten,
  as in the Docker driver; one scaled to zero loses its log lines and keeps its redaction values,
  so a wake and a token rotation still work). Logs of an instance that another process ended go
  after the retention too. The redactors' memory per instance is bounded by the secret limits but
  not part of these budgets.
- Usage is recorded by the process whose state change won, after the change; a crash or a lost
  answer between the two loses that run's usage (backlog I6-18).
- The supervisor asks for the current token every 30 s: a running app reads a rotated token
  within 30 s; the token issuer renews well before expiry (`apps::workload_tokens`).
- A Secret of an older release in `ruum-apps` stays until it is deleted by hand: the policy does
  not check existing objects (staging has none: the driver has been off since decision 0008-5).
  Records of an older release (without a seal) cannot start a pod; delete them by hand.
- Token rotations of one instance run one at a time (a lock in the process that redacts).
- The node's CRI log files hold the supervisor's output: exact matches of the values are
  redacted; hex, base64 and other forms pass (rule 7's known limit). A hostile app can write its
  own value into `/dev/termination-log` (the pod status, 4 KiB) and avoid the supervisor's cleanup
  by ignoring `SIGTERM`; it holds the value anyway.
- A crash of the starting process between the pod's creation and its seal leaves a pod whose
  launch never comes (the supervisor exits with 125 after 60 s) and an instance that ends.
- The seal of a record is AES-256-GCM with random nonces under one record key: rotate the key
  long before 2^32 seals (a second key id read for one release; not built yet).
- With two server replicas, each one's sandbox directory keeps its own map of deployment to
  instance, so a deployment can get one instance per replica.
- A crash right after the record of a start (before its pod and seal) leaves a record that the
  sweep ends after 60 s.
- Usage of the runtime is not metered yet on the server (card I-8); the server logs it at debug.

## The sandbox supervisor

Backlog I6-20, S-89; `docs/STEP3.md` "The sandbox supervisor" (design and threat model),
decision 0006 I. `ruum-supervisor` (`supervisor/`, package `ruum-supervisor`) is the first
process of a sandbox: it reads the launch data once (Docker: standard input; Kubernetes: `POST
/v1/launch` of the server's supervisor listener with the pod's projected token), starts the app
with the secret values in its environment, and redacts the app's standard output and standard
error with `ruum-core`'s redactor (compiled in from `crates/ruum-core/src/runtime/redact.rs`)
before the container runtime sees them. It is non-dumpable (the app cannot open
`/proc/1/fd/1`), forwards signals, reaps every child, sets `RLIMIT_NPROC` when the launch data
names a PID limit, empties `/dev/termination-log` before and after the app, and exits with the
app's exit code (125: a broken launch; 127: the command did not start). Static for
`*-unknown-linux-musl` (about 1 MiB), std, `nix` and `serde_json`, no `unsafe`.

The image (`supervisor/Dockerfile`, `FROM scratch`, the binary at `/ruum-supervisor`) is built
by hand until a card adds it to `.github/workflows/images.yml`.

```sh
cargo test -p ruum-supervisor                       # the core with /bin/sh apps, and the binary
rustup target add $(uname -m)-unknown-linux-musl     # once, for the Docker tests below
cargo test -p ruum-runtime --test conformance        # builds the static supervisor and uses it
```

## Builder

Card R-2b. `DockerBuilder` (`src/builder/`) implements `ruum_core::runtime::Builder` for
`runtime: node22` with the **fixed recipe** of decision 0006 G point 5 (spike S-2: rootless
BuildKit does not start under the driver's flags with runc, and not with runsc either, see
"Known limits of the builder"). Any other runtime is `unsupported_runtime`. No new dependency: the tar reader
and writer are the crate's own, `flate2`, `sha2` and `hex` come from the workspace.

### The recipe

1. **Checks.** `BuildInput::validate`; the runtime is `node22`; the build waits for a slot:
   at most `RUUM_BUILD_MAX_PARALLEL` builds at once (default 4) and at most
   `RUUM_BUILD_TENANT_MAX` of one tenant (default 2), so one tenant cannot hold every slot
   (backlog S-61). A build takes its tenant's slot before a global one.
2. **The archive** is unpacked in memory by W-2s's reader (`ruum_core::apps::archive::unpack`;
   decision 0006 H: no `..`, no absolute path, no `\`, no drive letter, no link or device;
   size limit: the build's disk limit and `RUUM_BUILD_OUTPUT_MAX`). `app.yaml` at the root must
   say `runtime: node22` and give `start` (the command that listens on `$PORT`); `build` is
   optional. A refused archive or manifest is `build_failed` with the reason. The source tar
   for the job adds each folder above a file once; the source's names with those folders may
   be at most 64 MiB in all, its entries (with those folders) at most 500,000, and the source
   tar itself at most `RUUM_BUILD_OUTPUT_MAX`, or the build fails (a deep path in every file, or
   many short paths with folders of their own, cannot make the builder copy each parent per file
   or write a tar of headers far larger than the archive; Kimi, run 12). Unpacking and the
   source tar run on the blocking pool.
3. **The job**: one sandbox container per build (`ruum-b-<build>`) from the base image, with
   the hardening of an app instance of the driver (read-only root, `cap-drop ALL`,
   `no-new-privileges`, user `10001:10001`, the driver's pids limit, the build's CPU and memory
   limits with swap = memory, `init`, private IPC, no published ports, the default seccomp and
   AppArmor profiles, the same OCI runtime, log driver `none`). Its only writable folders are
   two `tmpfs` mounts of the build's disk limit each: `/work` (`exec`, the source and its
   `node_modules`) and `/tmp` (`noexec`, npm's cache). No bind mount, no volume, no Docker
   socket, no secret: the environment is a fixed list (`HOME`, npm settings, the proxy
   variables, `RUUM_SOURCE_BYTES`, and `RUUM_BUILD_COMMAND` when the manifest has `build`). The
   script (`builder::RECIPE`) reads the source tar from standard input (`head -c`), runs
   `npm ci --ignore-scripts=false --foreground-scripts --no-audit --no-fund` (postinstall
   scripts run: they are the hostile part), then `sh -c "$RUUM_BUILD_COMMAND"` if set, then
   writes `tar -c -f - -C /work/app .` to standard output. npm, its scripts and the build
   command run with that output closed (`3>&-`): only the recipe's `tar` writes it. No value of
   the app is part of the script. The builder reads the container back before it starts it and refuses it when a flag
   or a limit differs.
4. **The network**: one per build (`ruum-build-<build>`), `internal`, `inhibit_ipv4`, IPv6 off,
   with only the egress proxy attached (alias and port from `DockerConfig::routes`, an
   `/etc/hosts` line in the job). It is registered at the proxy with the build's identity and
   the allow list **`registry.npmjs.org`** only (`BuilderConfig::npm_hosts`): packages come
   through the build's own registration. The routes a build reports are `BUILD_ROUTES` (the
   egress proxy).
5. **The output** leaves through the job's standard output, read by the builder under the byte
   cap `RUUM_BUILD_OUTPUT_MAX` (default 256 MiB): past it the job is killed and the build is
   `build_failed`. Why standard output and not `GET /containers/{id}/archive`: the archive API
   does not read `tmpfs` mounts reliably (and runsc keeps them inside the sandbox), and it would
   need the container to stay after its end; a stream is capped while it is read. The tar is
   read by the builder's own reader (`builder::output`), which never follows a link and never
   writes a file. **The rule**: regular files (mode cut to `0o755` bits: no setuid, setgid,
   sticky, group or world write), folders, and symbolic links whose target is relative and stays
   inside the output are kept; a name with `..`, an absolute name, an empty or `.` segment or a
   NUL, a name or link target over 4096 bytes, a link that is absolute or leaves the output, a
   hard link, a device, a FIFO and every other entry type are **dropped** and named in the build
   log (`ruum-build: dropped output entry ...`); an entry below a file or a link is dropped. The
   build **fails** on a broken tar, a tar without its end-of-archive marker (a cut stream or a
   broken attach connection never becomes an image with files missing), a long-name or PAX
   record whose entry is missing before that marker, a malformed PAX record, data after that
   marker, more than 500,000 entries (with the folders the builder adds above them), more than
   64 MiB of names (with those folders; both bounds are checked while reading), or a new layer
   over `RUUM_BUILD_OUTPUT_MAX`: the reading and the layer stay linear in the output, so a deep
   or a wide path cannot make the builder copy every parent (Kimi, run 12). A long name record
   is kept only up to 4097 bytes (and then dropped as too long). The link check is **lexical** (the names, not a disk): a `..` after a segment
   that is itself a link (`a/b/s -> ../..`, `a/b/t -> s/../..`) can end elsewhere when the
   kernel resolves it in the app's container. The builder never resolves a link and drops every
   entry below a link, so such a link points only inside the app's own image. File names are
   bytes: spaces, newlines, a leading `-`, `\` and `$(...)` stay data (backlog S-42). Reading
   the output and making the layer run on the blocking pool, so the build's timeout still ends
   the build.
6. **The image**, without a daemon (`builder::oci`): the base is `node:22-alpine` pinned by the
   digest of its multi-arch index (`NODE22_BASE_INDEX`, the index is embedded and checked), the
   platform of the engine (`GET /info`: `arm64` on the build server, `amd64` on staging and CI),
   read from `RUUM_REGISTRY/ruum-base/node22@<platform digest>`. One new layer: the output under
   `/app`, owner 10001, mtime 0, entries sorted, folders first, PAX records for long names,
   gzip with mtime 0. The config keeps the base's `Env`, sets `User: 10001`,
   `WorkingDir: /app`, no `Entrypoint`, `Cmd: ["/bin/sh", "-c", <start>]`, removes `Volumes`,
   `Healthcheck`, `OnBuild`, `ExposedPorts` and `StopSignal`, and carries the labels `ruum.app`,
   `ruum.tenant`, `ruum.build`, `ruum.managed=ruum-builder` (and the configured extra labels).
7. **The push** (`builder::registry::RepositoryPush`): to `ruum/<tenant>/<app>` of the build's
   identity only (the push has no repository parameter): the base layers by cross-repository
   mount from `ruum-base/node22` (a copy when the registry refuses the mount), the new layer and
   config, then the manifest **by digest**, once (a second manifest push of the same build is
   refused). An upload address (`Location`) is parsed and normalized as the request will send
   it: its scheme, host and port must be the registry's and its path must be under
   `/v2/<repo>/blobs/uploads/`, or the push stops; an address with `%2e`, `%2f`, `%5c`, a `\`,
   a fragment or user info is refused before parsing. Every answer is read under a cap
   (manifests and configs 4 MiB, a token 64 KiB, a copied base layer the size its manifest
   gives). With a credential (`BuilderConfig::credential`) and a `Bearer` challenge, the token
   is asked for `repository:<repo>:push,pull` and `repository:ruum-base/node22:pull`, and only
   from the registry's own origin or the realm `BuilderConfig::token_realm` names (an `https`
   URL, I-4): a challenge that names any other realm gets no request and no credential. The
   credential lives only in the builder process. The manifest push is the commit point: it runs
   after the build's timeout, with a bound of its own (60 s), so the timeout never cuts it after
   the registry stored the manifest.
8. **Cleanup and usage**: after every outcome (success, failure, timeout, and a build future
   that the caller drops: a drop guard spawns the cleanup and the usage record on the runtime)
   the container is removed by force, the network is unregistered at the proxy (three tries)
   and removed; the work data lives only in the job's `tmpfs`. A network whose unregistration
   fails is kept with its registration (never removed under a live registration) and the final
   cleanup tries again. A failed build pushes nothing. One
   `RuntimeUsage::Build` with the build's identity per build. The build log is a bounded tail
   (`MAX_BUILD_LOG_LINES`, 4096 bytes per line); a failure reason is cut at
   `MAX_BUILD_FAILED_REASON_BYTES`. `DockerBuilder::prune_images` removes the engine's images
   labelled `ruum.build` and `ruum.managed=ruum-builder` that no container uses (the driver
   pulled them); the registry's size cap and prune job are I-4's.

### Settings

| Variable | What |
|---|---|
| `RUUM_DOCKER_HOST`, `RUUM_RUNTIME_DRIVER`, `RUUM_REGISTRY` | As for the driver (`BuilderConfig::docker`). |
| `RUUM_BUILD_MAX_PARALLEL` | The most builds at once (default 4). |
| `RUUM_BUILD_TENANT_MAX` | The most builds of one tenant at once (default 2, at most the above). |
| `RUUM_BUILD_OUTPUT_MAX` | The byte cap of a build's output (default 256 MiB). |

The crate reads none of these: the server's wiring (I-4) does and builds `BuilderConfig`. The
egress proxy of the builds is `DockerConfig::routes[EgressProxy]`; the push credential is
`BuilderConfig::credential` (I-4 issues one scoped to pushes), and a token service that is not
on the registry's own origin is `BuilderConfig::token_realm`.

### Docker API calls of the builder (for I-4's socket proxy)

Every object carries `ruum.app`, `ruum.tenant`, `ruum.env`, `ruum.deployment`, `ruum.build` and
`ruum.managed=ruum-builder`.

| Method and path | When |
|---|---|
| `GET /_ping`, `GET /version`, `GET /info` | connect (version, the platform, is `runsc` registered) |
| `GET /images/{RUUM_REGISTRY}/ruum-base/node22@{digest}/json` | each build: is the base image local |
| `POST /images/create?fromImage={RUUM_REGISTRY}/ruum-base/node22@{digest}` | pull the base image (a local registry reference only) |
| `POST /networks/create` | each build |
| `GET /networks/{name}` | after create (read back `internal`, `inhibit_ipv4`, IPv6, the subnet), after attaching the proxy (its address, nothing else attached) |
| `POST /networks/{id}/connect` | attach the egress proxy |
| `POST /containers/create?name=ruum-b-{build}` | each build |
| `GET /containers/{name}/json` | after create (read back every flag and limit) |
| `POST /containers/{name}/attach?stream=1&stdin=1&stdout=1&stderr=1` | before start (source in, output and log out) |
| `POST /containers/{name}/start` | each build |
| `POST /containers/{name}/kill?signal=SIGKILL` | the output went over the cap |
| `POST /containers/{name}/wait` | the exit code |
| `DELETE /containers/{name}?force=1&v=1` | after every outcome |
| `POST /networks/{name}/disconnect` | detach the egress proxy (after the unregistration) |
| `DELETE /networks/{name}` | after every outcome |
| `GET /images/json?filters={"label":["ruum.build","ruum.managed=ruum-builder",...]}` | `prune_images` |
| `DELETE /images/{id}` | `prune_images` (not forced) |

Outside the engine: the registry (`GET` and `HEAD` under `/v2/ruum-base/node22/`, `POST`,
`PUT`, `HEAD` under `/v2/ruum/<tenant>/<app>/`, and the token realm of a `Bearer` challenge)
and the egress proxy's control listener (`PUT` and `DELETE /v1/networks/{id}`).

### Tests

```sh
cargo test -p ruum-runtime --lib builder                                    # tar, OCI, push (wiremock), slots
cargo test -p ruum-runtime --test build -- --nocapture                      # runc, stand-in egress
RUUM_SANDBOX_FULL=1 cargo test -p ruum-runtime --test build -- --nocapture  # + R-2e's proxy and the hostile network case
RUUM_SANDBOX_FULL=1 RUUM_RUNTIME_DRIVER=runsc cargo test -p ruum-runtime --test build -- --nocapture
```

`tests/build.rs` labels everything `ruum.test=r2b-<random>` and removes only that (backlog
T-53). It mirrors the base image of the engine's platform from Docker Hub into its own
`registry:2` (blobs cached in `CARGO_TARGET_TMPDIR`). The fixtures are `testdata/build/`.
`RUUM_BUILD_CASES` runs some cases only (the mutation checks); it is refused when `CI` is set.

### Known limits of the builder

- Rootless BuildKit (and so Railpack) does not start under the driver's flags with runsc
  either: `rootlesskit` makes the user namespace inside gVisor, but `newuidmap` is a setuid
  helper, which `no-new-privileges` and `cap-drop ALL` stop (`fork/exec /usr/bin/newuidmap:
  operation not permitted`; probe of 2026-11-18, `moby/buildkit:v0.23.2-rootless`). The fixed
  recipe stays the builder.
- The base image's platform image must be in the local registry (`ruum-base/node22@<digest>`);
  the builder never pulls from the internet. A different pin (`BuilderConfig::base_index`) needs
  the whole index there.
- The output and the new layer are held in memory (at most `RUUM_BUILD_OUTPUT_MAX` each per
  build, times `RUUM_BUILD_MAX_PARALLEL`; the uncompressed layer is dropped once its digest is
  known).
- A network whose unregistration fails at both cleanups is left, still registered, until an
  operator removes it: there is no sweep of build networks yet (T-53 follow-up, for I-4). The
  test label sweep of objects older than a day is missing in both suites too.
- When the registry stores the manifest but its answer fails (or the caller drops the build
  during the manifest push), the build reports an error although the manifest is in the
  build's own repository: by digest only, never tagged, never referenced by a deployment.
- The wait for a build slot is not part of `BuildInput::timeout`: the caller (I-4) bounds it.
- Under runsc the pids limit bounds the sandbox's host threads: a fork bomb ends the whole job
  (the build fails without the job's own report).
- A build without `build` in `app.yaml` runs no build script (no `npm run build` by default).
- The job needs a `package-lock.json` (`npm ci`); without one the build fails with npm's
  message.
- The `NODE_USE_ENV_PROXY=1` variable makes Node 22 print an "experimental" warning in the log.
