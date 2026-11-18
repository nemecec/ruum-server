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
   optional. A refused archive or manifest is `build_failed` with the reason.
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
   broken attach connection never becomes an image with files missing), data after that marker,
   more than 500,000 entries, or more than 64 MiB of names (with the folders above them): the
   reading and the layer stay linear in the output, so a deep path cannot make the builder copy
   every parent. The link check is **lexical** (the names, not a disk): a `..` after a segment
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
