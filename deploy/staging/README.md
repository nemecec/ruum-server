# Staging as code

Staging of Ruum on Hetzner (`fsn1`), as decided in decision 0007 (budget EUR 30-50 per month). The
design is `panel/staging-plan.md` with every blocker and must item of its two reviews fixed (table
at the end). Nothing in this folder creates a cloud resource unless you run it with
`RUUM_STAGING_APPLY=yes`.

## Shape

- 3 x cx23 (2 vCPU, 4 GB) Talos nodes, all control plane and worker; one lb11 load balancer
  (TCP 80/443 to NodePorts 30080/30443, proxy protocol); one private network 10.20.0.0/16; one
  firewall, attached when each server is created.
- CloudNativePG (primary and replica on local disks), WAL and daily base backups to Scaleway with
  the backup writer key; SpiceDB, Zitadel, NATS JetStream, Mailpit; Caddy as the edge (with the
  web UI); ruum-server.
- Zitadel sends email through Scaleway Transactional Email (see "Email"); ruum-server still sends
  to Mailpit inside the cluster.
- About EUR 38 per month (the priced plan of `up.sh`); EUR 45.6 with a fourth node, the last step
  within EUR 50. `up.sh` refuses a plan above EUR 50.

## Where things run

Everything runs on the person's Mac (decision of the person, 2026-11-11). The staging admin
credentials (kubeconfig, talosconfig, the OpenTofu state passphrase, the cluster secrets) live only
there, in `~/.config/ruum/`. The build server is for builds and tests only: its agent user is in
the `docker` group, which is root on that host, so no user separation there could protect them.

- The scripts run in the Mac's own bash (the macOS bash 3.2 is enough). They read
  `~/.config/ruum/secrets.env` one name at a time and never source it (`lib.sh`).
- Each step runs `steps.sh` in the **tools container** with Docker Desktop: `docker run --rm`,
  no capabilities, no new privileges, the person's user ID. The checkout is mounted read-only.
  The only writable mount is the work folder `~/.config/ruum/staging` (0700): kubeconfig,
  talosconfig, the deployer kubeconfig and the plans. The copies of the OpenTofu roots and their
  providers live in the container's `/tmp` and go with it.
- Secrets go to the container on its stdin as `NAME=value` lines: never on a command line (not in
  `ps`, not in `docker inspect`), never in a file, never printed. Each step gets only the names
  of its list.
- The Talos and Kubernetes APIs are reached through an SSH tunnel over the build server (next
  section). The build server relays encrypted bytes only; it holds no staging credential.

## Admin access

Decision of the person, 2026-11-11: the Mac's public address changes too often for a firewall
rule, so admin access goes through the build server, whose public IPv4 is fixed.

- **Firewall.** The Talos API (tcp 50000), the Kubernetes API (tcp 6443) and ICMP are open only
  to the build server's public IPv4 as one /32. `steps.sh admin_cidrs` reads it at plan time from
  the Hetzner API (server `ruum-buildbox`); it is not in the code. `check-plan.sh` accepts exactly
  that /32 and nothing wider, no IPv6.
- **Tunnel.** `lib.sh stg_tunnel_up` opens an SSH local forward from the Mac, as the build-server
  user `ruumtunnel`, around the steps that need the cluster (`up.sh --apply` before OpenTofu,
  `deploy.sh`, `status.sh`, `secrets-push.sh`, `tool.sh`), and closes it when the script ends.
  Ports on `127.0.0.1` of the Mac: `16443` to stg-1:6443, and `1500N` to stg-N:50000. The ssh
  options: only the key `~/.config/ruum/staging_tunnel_ed25519`, only the host key in
  `~/.config/ruum/staging_tunnel_known_hosts` (`StrictHostKeyChecking=yes`),
  `ExitOnForwardFailure=yes`, no command (`-N`), no agent, no X11, no tty, no user ssh config.
  The node targets come from the Hetzner API (`steps.sh tunnel-targets`).
- **TLS through the tunnel.** The kubeconfig has `server: https://127.0.0.1:16443` and
  `tls-server-name: kubernetes`; the talosconfig has the endpoints `127.0.0.1:1500N` and the
  nodes by their private address (the endpoint's apid relays to the node over the private
  network). Talos v1.14 puts `127.0.0.1` into both the apid and the kube-apiserver certificate
  by itself, so the machine config does not change. The tools container cannot reach the Mac's
  `127.0.0.1`, so `tunnel-relay.sh` listens on the same ports in the container and relays them to
  `host.docker.internal` (Docker Desktop): the same files work on the Mac and in the container.
- **The build-server user.** `ruumtunnel` has no password and the shell `/usr/sbin/nologin`.
  Its only key is in `/etc/ssh/authorized_keys/ruumtunnel` (root, 0644):
  `restrict,port-forwarding,permitopen="<node>:6443",permitopen="<node>:50000",...,command="/bin/false"`
  with the nodes' public IPv4 (the build server is not on the private network). sshd turns TCP
  forwarding off for every other user and allows ruumtunnel local forwards to those ports only
  (`Match User ruumtunnel`: `AllowTcpForwarding local`, `PermitOpen`, `X11Forwarding no`,
  `AllowAgentForwarding no`, `PermitTTY no`, `ForceCommand /bin/false`).
  `ops/buildbox-tunnel-user.sh` sets this up, checks `sshd -t` and the effective settings
  (`sshd -T`) before it reloads sshd, and is idempotent.
- **Setup, once, and again when the node addresses change** (a recreate):
  `deploy/staging/ops/tunnel-setup.sh`. It makes the tunnel key when it is missing (never
  printed), runs `ops/buildbox-tunnel-user.sh` on the build server as the admin user
  (`ssh -i ~/.config/ruum/buildbox_admin_ed25519 ruumadmin@<build server> 'sudo bash -s -- <key> <node IPs>'`),
  records the build server's host key that the script prints, and opens the tunnel once.
- **OpenTofu and Talos** (backlog O-21). The Talos resources of `tofu/cluster`
  (`talos_machine_configuration_apply`, `talos_machine_bootstrap`) reach the nodes through the
  tunnel: `endpoint = 127.0.0.1:1500N` (relayed into the tools container), `node` = the node's
  public IPv4, so the endpoint's apid relays to itself. The provider contacts a node only when it
  creates or changes such a resource; a plan never does. A resource that the plan does not create
  or change keeps the endpoint of the state (`steps.sh plan_cluster`: `tofu show -json` piped into
  `talos-access.sh state-endpoints`, which prints only the endpoints, into the variable
  `talos_endpoints`), so a plan without a Talos change shows no change; when the plan creates or
  changes one, the root is planned again with the tunnel endpoint for exactly those resources. A
  precondition allows only the node's own tunnel port or its own public address.
  `up.sh --apply` reads how the plan reaches the Talos API (`steps.sh talos-access`):
  - `none` or `tunnel` (a machine config change on running nodes): it opens the tunnel, then
    applies. `steps.sh apply` refuses a Talos change whose node port is not open.
  - `create` (a fresh create, a new node or a replaced server): the new public addresses are not
    in the `ruumtunnel` key line yet. Phase 1 is a targeted apply of the servers and what they need
    (`steps.sh apply servers`: the whole persistent root, then the cluster root with
    `-target=hcloud_server.node`, checked and priced together); then
    `ops/tunnel-setup.sh --before-bootstrap` puts the new addresses on the build server and waits
    up to 10 minutes for their Talos API (maintenance mode). Then the tunnel opens and the full
    plan is applied: the machine configs and the bootstrap through the tunnel.
  - `bootstrap` (servers exist, the bootstrap does not, for example after a create stopped
    half-way): as `create` without phase 1. `ops/tunnel-setup.sh --before-bootstrap` runs here
    too, because a create that stopped after phase 1 has not put the new addresses on the build
    server yet (the script is idempotent).
  - `setup` (the servers and the bootstrap exist, but the plan creates the machine config of a
    node, for example after adding a node stopped after phase 1): `ops/tunnel-setup.sh
    --before-bootstrap` runs before the tunnel opens, so the build server forwards to the new
    address. The tunnel still checks stg-1's Kubernetes API (Kimi review of run 13).
  After a bootstrap, `up.sh` waits up to 15 minutes until stg-1's Kubernetes API answers through
  the tunnel (`stg_tunnel_wait_kube`); until then the tunnel does not probe that forward
  (`STG_TUNNEL_NO_KUBE_CHECK`). The full apply refuses a plan that creates servers.

Plan JSON is scrubbed in a pipe (`plan-scrub.jq`: no variables, no prior state, no sensitive or
Talos values) before it is written, so no plan file holds the cluster secrets (code review F-5).
The binary plan files are encrypted by OpenTofu with the state passphrase. A plan without
`--apply` takes no state lock, so it writes nothing to the state bucket.

OpenTofu state: `ruum-staging-tfstate` at Scaleway (kept on destroy), encrypted by OpenTofu with
`TOFU_STATE_PASSPHRASE` before it leaves the Mac. Keep a copy of the passphrase in the password
manager: without it the state cannot be read, and only `destroy.sh --sweep` can remove staging.

## The tools image

`tools-image.sh` builds one local image, `ruum-staging-tools:<16 hex>`, the first time a step
needs it (about 2.5 minutes, about 760 MB):

- the base `TOOLS_BASE_IMAGE` of `versions.env` (Alpine, pinned by digest) with bash, coreutils,
  curl, jq, openssl and socat (`tunnel-relay.sh`);
- OpenTofu, talosctl, kubectl, helm, helmfile, cosign, hcloud and hcloud-upload-image, each
  downloaded by `setup.sh` at the version of `versions.env` and checked against its SHA-256 (the
  same pins as CI);
- the OpenTofu providers of both roots at the versions and hashes of their lock files, in a
  local mirror; `tofu init` in the container uses only that mirror.

The steps need several tools together in one process (for example `kubectl port-forward` and
curl, helmfile with helm and kubectl, OpenTofu with the Talos provider and jq), so it is one
image, not one image per tool. The tag is a hash of `tools-image.sh`, `setup.sh`,
`versions.env` and the two lock files: a version change builds a new image and removes the old
one, so the Mac keeps one. The build is `docker run` and `docker commit`, not `docker build`,
so it leaves no build cache. `tools-image.sh clean` removes it.

`deploy/staging/tool.sh` runs any command in that container with the work folder mounted and
`KUBECONFIG`/`TALOSCONFIG` set, for example `deploy/staging/tool.sh kubectl get pods -A`, with the
admin tunnel open while it runs (`--no-tunnel` skips it). The command gets no value from
`secrets.env`; only the separate container that reads the tunnel targets gets `HCLOUD_TOKEN`.

## Commands

| Command | What it does |
|---|---|
| `deploy/staging/up.sh [--out FILE]` | The priced plan: `tofu plan` of both roots, `check-plan.sh`, `price.sh`, and the platform preflight. Changes nothing, takes no state lock. |
| `RUUM_STAGING_APPLY=yes deploy/staging/bootstrap-state.sh` | Creates the state bucket once. |
| `RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply` | Generates the missing `STG_*` secrets (`secrets-init.sh`), uploads the Talos snapshot if missing, plans, checks, prices; for new servers applies them first; for new servers, a missing bootstrap or a new node's machine config runs `ops/tunnel-setup.sh --before-bootstrap` ("Admin access", O-21); opens the admin tunnel, applies the exact plan (the Talos resources through the tunnel), checks that every node has empty user data, writes the talosconfig, waits for the Kubernetes API after a bootstrap, writes the kubeconfig, installs the platform, and configures Zitadel. |
| `... up.sh --apply --restore-from <generation>` | The same, with Postgres restored from an older backup path (disaster-recovery test). |
| `RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply --infra-only` | Only the plan, the checks, the apply, the talosconfig and the kubeconfig. |
| `deploy/staging/ops/tunnel-setup.sh [--before-bootstrap]` | The admin tunnel: the key (only if missing), the `ruumtunnel` user on the build server, the host key, a test ("Admin access"). `--before-bootstrap` (up.sh runs it in a fresh create, before a missing bootstrap and before a new node's machine config): the test waits for the Talos API of new nodes and skips stg-1's Kubernetes API. |
| `deploy/staging/deploy.sh <sha>` | Deploys a green commit of main by digest: checks it on the Mac (`gh`), then writes a release request that the deploy controller checks again by itself (`cosign`, GitHub) and runs ("The deploy controller"); `--rollback` to go back to the previous release on purpose, `--rollback-older` stops with the admin path to go back further ("Going back past the previous release"); one deploy at a time (Lease `ruum/ruum-deploy`); `--acme zerossl` to switch the CA. |
| `deploy/staging/status.sh [--checks]` | What exists, its price, orphans, nodes, pods, backups, deployed commit; with `--checks` also the smoke checks and Zitadel drift. |
| `deploy/staging/status.sh --netpol` | The network policy probe (`ops/netpol-probe.sh`): an unknown pod reaches no platform port; ruum-server reaches only Postgres, SpiceDB HTTP, NATS and Mailpit; the edge reaches only ruum-server, Zitadel and the app gateway; the gateway reaches only the internal listener and Postgres among the platform ports; a sandbox (runtime class gvisor, `ruum-apps`) reaches only the egress proxy. Not read-only (a temporary namespace, ephemeral containers, a sandbox pod). |
| `tool.sh bash deploy/staging/ops/gvisor-check.sh [--class-only]` | The gVisor extension on every node; without `--class-only` also a pod of the runtime class gvisor and a control pod on each node (card I-6). Not read-only (short pods in `ruum-apps`). |
| `deploy/staging/ops/smoke-apps.sh` | The smoke checks of the app runtime (card I-6), on the Mac; also in `status.sh --checks`. |
| `deploy/staging/destroy.sh --plan` | Three lists (removed, kept on purpose, never touched) and the state compared with the labelled resources. Read-only. |
| `deploy/staging/destroy.sh` | The same, then asks you to type `destroy ruum-staging`. |
| `deploy/staging/destroy.sh --sweep` | When the state is lost: deletes by label `env=staging`, only the staging names (nodes, their IPs, LB, network, firewall, placement group, and the rrsets of `rrsets.json`), never the build server or the zones. |
| `deploy/staging/tests/run.sh` | The tests of this folder (also CI job `staging`). On the Mac, helm and tofu come from the tools image. |
| `deploy/staging/tests/server-env.sh` | The guard against drift between the server and the chart: every `RUUM_*` setting of the chart and of `ruum/ruum-server-env` is one the server reads, and every setting it needs in production is set. The list is `crates/ruum-server/env-names.txt`, checked against the code. Change the list when the server gets a new setting. |
| `deploy/staging/tool.sh COMMAND...` | One command in the tools container (kubectl, talosctl, helm, the `ops/` scripts below). `--port N` publishes a port on `127.0.0.1` of the Mac. |
| `deploy/staging/tools-image.sh [build \| clean]` | Builds or removes the tools image (the steps build it when it is missing). |
| `tool.sh bash deploy/staging/ops/upgrade-talos.sh [--check]`, `... ops/upgrade-k8s.sh` | Upgrades, one node at a time, after a version or schematic change in `versions.env`; `--check` only reports each node's version, schematic and extensions. |
| `tool.sh bash deploy/staging/ops/restore-test.sh` | Monthly restore test into a temporary cluster. |
| `tool.sh bash deploy/staging/ops/check-capacity.sh` | The N-1 sizing rule (below). |
| `ops/check-backup.sh` | Bring-up check (`up.sh --apply` runs it after the platform): a forced WAL switch is archived into the COMPLIANCE-locked bucket and an on-demand base backup completes (code review F-7). |

## Before the first apply (the person)

1. **Backup writer key.** Scaleway console, project `ruum-staging`: IAM, Applications, Create
   application `ruum-staging-backup-writer`; policy on the project with
   `ObjectStorageObjectsRead`, `ObjectStorageObjectsWrite` and `ObjectStorageBucketsRead` only
   (no delete, no email). Generate an API key for it (Object Storage: yes, preferred project
   `ruum-staging`) and put it into `secrets.env` as `SCW_BACKUP_ACCESS_KEY` and
   `SCW_BACKUP_SECRET_KEY`. `up.sh` stops until both are present.
2. **Allow-list.** `STG_ALLOWED_EMAILS=you@example.com` in `secrets.env`: the addresses that may
   sign in to staging. Optional: `STG_ALLOWED_UNVERIFIED_EMAILS` for a Microsoft address (see
   "Sign-in test").
3. **SMTP key.** The API key of the Scaleway IAM application `ruum-staging-smtp`, whose only
   permission is `TransactionalEmailEmailSmtpCreate` on project `ruum-staging`
   (`docs/setup/ACCOUNTS.md`, step 3). Put its secret key into `secrets.env` as
   `SCW_SMTP_SECRET_KEY` (and the access key as `SCW_SMTP_ACCESS_KEY`, for the record; SMTP does
   not use it). `up.sh` stops until `SCW_SMTP_SECRET_KEY` is present; `secrets-init.sh` never
   generates it.
4. **Zone delete protection.** Hetzner console, DNS: turn on delete protection for `ruum.space`
   and `ruumapps.net`. The code never manages the zones, but the token could delete them.
5. **Docker Desktop** on the Mac, running. The first step builds the tools image (above).
   The admin tunnel needs `host.docker.internal` (Docker Desktop has it).
6. Optional: `STG_ACME_EMAIL` for ZeroSSL (the trusted CA for the sign-in test).

## Secrets

The Mac's `secrets.env` holds everything; the cluster gets one Secret per consumer and namespace
(`k8s/apply.sh`). `HCLOUD_TOKEN`, the full Scaleway key, the state passphrase and the Stripe keys
never go to the cluster (`secrets-push.sh` refuses them; `check-plan.sh --repo` and the tests
check the files).

| Secret | Holds | Read by |
|---|---|---|
| `data/pg-backup-s3` | backup writer key | CNPG (Barman Cloud plugin) |
| `data/pg-role-{ruum,ruum-app,zitadel,spicedb}` | database passwords (`ruum` owns the database ruum; `ruum_app` is the serving role) | CNPG managed roles |
| `auth/zitadel-masterkey`, `auth/zitadel-db` | master key, DSN | Zitadel |
| `auth/zitadel-smtp` | SMTP login at Scaleway: `user` (the project ID, `SCW_DEFAULT_PROJECT_ID`), `password` (`SCW_SMTP_SECRET_KEY`) | Zitadel (setup of a new instance), `ops/zitadel-bootstrap.sh` |
| `authz/spicedb` | preshared key, DSN | SpiceDB |
| `ruum/ruum-server-env` | DSN of `ruum_app` (data rights only: no DDL, read-only `_sqlx_migrations`), Hetzner S3 key, SpiceDB key, token and content keys, allow-list | ruum-server (serving pods) |
| `ruum/ruum-migrate-db` | DSN of the owner `ruum` (DDL) | the migration Job only (created by the deploy controller; the deployer cannot reach it, backlog O-34) |
| `ruum/ruum-oidc` | OIDC client ID (`client-id`) and the Zitadel IdP IDs of Google and Microsoft (`idp-google`, `idp-microsoft`, for `RUUM_OIDC_IDP_*`), made by `ops/zitadel-bootstrap.sh` | ruum-server |
| `ops/blob-copy` | Hetzner S3 key, backup writer key, rclone crypt password and salt | the nightly copy |
| `nats/nats-auth` | the ruum user's password | NATS |

The Google and Microsoft keys go only into Zitadel's database (through the bootstrap), never into
a Secret. The SMTP key is in `auth/zitadel-smtp` and, encrypted, in Zitadel's database; never the
agents' full Scaleway key (`check-plan.sh --repo` refuses `SCW_SECRET_KEY` in a cluster file). The generated values (`STG_*`) are in `secrets.env` too, so a restore after the loss of
the whole cluster can read Zitadel's data.

Backups: HTTPS in transit. At rest, CNPG backups have only Scaleway's provider-side encryption;
the blob copy is encrypted by rclone crypt with a key that lives off the cluster.

## Email

Zitadel sends its email (the email check of a Microsoft sign-in, backlog O-20) through Scaleway
Transactional Email: `smtp.tem.scaleway.com:587`, STARTTLS, sender `Ruum (staging)
<no-reply@notify.staging.ruum.space>`. The sending domain is verified in project `ruum-staging`
(region `fr-par`) with SPF, DKIM, DMARC and MX in public DNS.

- Port 587: Hetzner blocks outgoing ports 25 and 465 on new accounts. Scaleway also listens on 2587
  (STARTTLS) and 2465 (implicit TLS). Zitadel's `tls: true` tries implicit TLS first and, when the
  server answers in plain text, upgrades with STARTTLS and checks the host name; it logs a warning
  "could not connect using normal tls" for each connection on 587. That warning is expected.
- No egress network policy exists (only ingress policies), and the Hetzner firewall has inbound
  rules only, so Zitadel reaches port 587 without a rule. If egress policies are added, allow
  `auth/zitadel` to TCP 587 out (and DNS).
- `DefaultInstance.SMTPConfiguration` in `k8s/values/zitadel.yaml` applies only when Zitadel creates
  an instance. For the instance that exists, `ops/zitadel-bootstrap.sh` adds or updates the SMTP
  provider through the admin API (`/admin/v1/email/smtp`) and activates it, which makes the old
  Mailpit provider inactive. `zitadel-bootstrap.sh check` (also in `status.sh --checks`) reports
  drift: no active provider, another host, TLS off, another sender or user.
- Only the SMTP part: `deploy/staging/tool.sh bash deploy/staging/ops/zitadel-bootstrap.sh smtp`.
  One test email: `... zitadel-bootstrap.sh test-email you@example.com`.
- Mailpit stays deployed (ruum-server and laptop parity) but is not Zitadel's provider.

## Edge and certificates

- Caddy as a DaemonSet, one edge on every node (backlog O-26, design 12.7). Certificates, ACME
  accounts and challenge tokens are in Kubernetes Secrets of the `edge` namespace (the Caddy
  module `deploy/images/caddy-secret-storage`, locks as Leases), never in a bucket that
  ruum-server can read. A restart loads them and issues nothing; any edge answers the HTTP-01
  challenge that another one started. The ServiceAccount `ruum-edge` may touch only the Secrets
  and Leases of `edge` (`charts/platform/templates/edge-rbac.yaml`).
- An edge update starts the new pod on a node before it stops the old one (maxSurge 1,
  maxUnavailable 0), so every node keeps a ready edge for the load balancer's TCP check
  (`externalTrafficPolicy: Local`). Stopping: a 5 s pause, then Caddy's `grace_period` of 30 s.
- The ACME CA is never production Let's Encrypt, so staging never spends production's quota:
  `letsencrypt-staging` during bring-up (untrusted by browsers; HSTS max-age 300 then), `zerossl`
  for the sign-in test (`deploy.sh <sha> --acme zerossl`). CAA allows `letsencrypt.org` and
  `sectigo.com` (ZeroSSL), no wildcard.
- On-demand TLS only in the `*.content.staging.ruumapps.net` block; Caddy asks ruum-server's
  internal listener (port 8493, reachable only from the edge) and fails closed.
- `auth.staging.ruum.space`: sign-in and OIDC paths are public; the console and the admin APIs
  answer 403 to every public address (`adminSources` is empty). The person reaches the console
  through the cluster: `deploy/staging/tool.sh --port 8080 kubectl -n auth port-forward
  --address 0.0.0.0 svc/zitadel 8080`.

## Deploy rules

- By digest only: the chart accepts only `ghcr.io/nemecec/ruum-{server,edge}@sha256:...`.
  `deploy.sh` checks CI, the digest and the provenance attestation of `images.yml` on
  `refs/heads/main`, then writes a release request. The deploy controller checks the request by
  itself ("The deploy controller"), and `ops/deploy-chart.sh` in the controller validates every
  argument again (commit 40 hex, issuer from a fixed list, a plain email), checks that the tag
  `<commit>` is that digest, verifies the attestation again with `cosign`, and gives helm a values
  file, never `--set` (code review F-3).
- Migrations follow **expand and contract** (design 12.7, decision 42): a release only adds; a
  removal comes one release later. Old and new pods serve side by side in a rollout, and a
  failed rollout rolls the image back (`--rollback-on-failure`, Helm 4) but not the schema, so
  the previous image must work on the new schema. `deploy.sh` takes an on-demand CNPG backup
  before a release with new migrations.
- **Migrations are their own step** (backlog O-25): `ops/deploy-chart.sh` (in the deploy
  controller) renders the Job of
  `charts/ruum/templates/migrate-job.yaml` with the new server image, which runs
  `ruum-server migrate` under a Postgres advisory lock, and waits for it. If it fails, nothing is
  rolled out and the old release keeps serving; the full Job log goes to the controller's log (admin:
  `deploy/staging/tool.sh kubectl -n ruum-deploy logs deploy/deploy-controller`); the request
  that the deployer reads gets its last 100 lines, with secret-looking values masked, only when
  the step fails (backlog O-50: the deployer reads no pod log). A failed rollout puts helm's error
  and the warning events of ruum into the request. The Job stays for a week. The serving pods run with `RUUM_MIGRATE=false` and do not start on a database that lacks
  a migration of their release.
- **Rolling update**: ruum-server runs two replicas spread over the nodes, `maxUnavailable: 0`,
  a disruption budget of one, a readiness check every 5 s. Stopping: a 10 s `preStop` pause
  (the pod leaves the Service first), then SIGTERM: no new connections, open requests may finish
  for `RUUM_SHUTDOWN_GRACE_SECONDS` (35 s, longer than the 30 s long poll); longer WebDAV
  transfers and downloads are cut and the client retries.
- **Two database roles** (Kimi review of O-25, findings 1 and 5). The serving pods log in as
  `ruum_app`: select, insert, update and delete on the tables, select only on
  `_sqlx_migrations`, no `CREATE`, owns nothing. Only the migration Job logs in as the owner
  `ruum`, with the DSN in `ruum/ruum-migrate-db`, which the serving pods do not mount. CNPG makes
  both roles (managed roles); `k8s/apply.sh` runs `k8s/sql/ruum-runtime-grants.sql` on the
  primary on every platform step (idempotent; it fails when `ruum_app` could still change the
  schema). After each run the migration step takes the write rights on `_sqlx_migrations` from
  `ruum_app` again and checks its rights (`RUUM_DB_RUNTIME_ROLE`). A serving pod with
  `RUUM_MIGRATE=false` and no `DATABASE_URL` refuses to start; it never falls back to memory.
- **One deploy at a time** (finding 3). `ops/deploy-chart.sh` holds the Lease `ruum/ruum-deploy`
  (`ops/deploy-lock.sh`) from before it reads the live release until `helm upgrade` has
  finished. A second deploy stops with the holder's name and the lock's end. After it takes the
  lock, it reads `ruum-release` again and stops when the live commit is not the one `deploy.sh`
  checked (`--expect-deployed`). The Lease lasts 30 minutes and is renewed after the backup and
  after the migration step; a deploy that lost it stops before its next step. A crashed deploy
  leaves the lock until it expires (or the admin clears `spec.holderIdentity`).
- An older commit than the deployed one needs `--rollback`: the same path; its migration step
  has nothing to do. The controller finds the order itself (GitHub's compare API) and refuses an
  older commit without a rollback. **`--rollback` goes back one release only** (finding 2): the target must be
  the release that was live before the current one (`previous` in the ConfigMap `ruum-release`),
  because design 12.7 promises only that the previous release runs on the new schema. An older
  target needs `--rollback-older`, which prints a warning: check every migration since the target
  for a contract step that it still uses. The deploy controller never runs it (Kimi review of
  O-34, finding 1): an admin does ("Going back past the previous release"). The first deploy with this rule records `previous`; until
  then a rollback needs `--rollback-older`. The chart and the migration Job always come from the
  deploy controller's image (the commit of main that the last platform step installed), and the
  platform from the checkout that runs the platform step; never from the target commit. **The rollback floor is backlog O-25** (the commit that added the migration
  step and the edge's certificate storage). An older commit has no `ruum-server migrate` (its
  Job would fail on a usage error) and its edge image has no `kubernetes_secret` storage module
  (Caddy would not load the Caddyfile). `deploy.sh` refuses such a commit before it touches the
  cluster (`ops/release-floor.sh`). To go back further, revert on main and deploy the revert.
- ruum-server keeps two limits in memory, per process. The ask limit of on-demand TLS
  (`server.tlsAskNewHostsPerHour`, 20) is for all replicas together: the chart gives each
  replica its share (20 / 2 = 10). The surge pod of a rollout adds one share for a short time,
  and a host that one replica allowed counts again on another. The rate limits of
  `ruum_api::limits` are per process, so N replicas allow N times as much. Shared counters are
  backlog O-28.
- Before the first deploy with these steps, apply the platform (`k8s/apply.sh`): it installs the
  deploy controller and its rights, the edge's ServiceAccount and Role, and the network policy
  rule that lets the migration Job reach Postgres. `deploy-chart.sh` stops when the controller
  cannot create Jobs.

## The deploy controller (backlog O-34)

Decision 0008-2 (option a). Before it, the deployer could create Pods, Jobs, Deployments and
DaemonSets in `ruum` and `edge`, and a pod mounts any Secret of its namespace, so the deployer
could read every Secret there (Kimi review of O-25, finding 1). Now:

- **The controller** (`deploy/images/deploy-controller`, Go, standard library only) runs in its
  own namespace `ruum-deploy` (Pod Security restricted, default deny ingress) as the
  ServiceAccount `ruum-deploy/deploy-controller`, one pod (`Recreate`). Its image
  `ghcr.io/nemecec/ruum-deploy-controller` is built, scanned and attested by `images.yml` like the
  other two, with kubectl, helm and cosign from `setup.sh` (the pins of `versions.env`), the chart
  `charts/ruum` and the deploy scripts. The platform step installs it by digest: `secrets-push.sh`
  takes the checkout's commit (it must be on `origin/main`; `RUUM_DEPLOY_CONTROLLER_COMMIT`
  picks another), and `k8s/apply.sh` resolves the tag and verifies the provenance with `cosign`
  before anything changes.
- **The deployer only writes release requests**: a ConfigMap in `ruum-deploy` with the label
  `ruum.space/release-request=true` and the keys `commit`, `server`, `edge` (the two digests),
  `expectDeployed` (the live commit `deploy.sh` read) and `rollback` (`none`, `previous` or
  `older`), optionally `acme` and `acmeEmail` (`ops/release-request.sh`). It may create and read
  them, never change or delete them. It reads `ruum/ruum-release` and the state of the workloads.
  It creates no workload anywhere, uses no pod subresource (no `pods/log`, no exec), reads no
  Secret, gets no token, holds no Lease and creates no CNPG Backup
  (`charts/platform/templates/deployer-rbac.yaml`).
- **The controller checks each request itself**, oldest first, one at a time: every field (the
  patterns of `ops/image-check.sh`, no other key); for each image, the tag `<commit>` is that
  digest, the provenance attestation of `images.yml` on `refs/heads/main` (`cosign`), and the
  attested image's revision label is the commit; with GitHub's API (no token, at most five calls)
  that the commit is on main, that it has the migration step and the edge storage (the O-25
  floor), and its order against `expectDeployed`: an older commit needs a rollback, a newer one
  must not ask for one; migrations in between (or a cut file list) mean an on-demand backup. A
  request that waited more than five minutes before the controller saw it is refused, so an old
  request never deploys by surprise.
- **Then it runs `ops/deploy-chart.sh`** with its own ServiceAccount: the deploy lock, the live
  release check, the rollback rule (previous release only; the controller refuses a request with
  `rollback: older` before any other check), the backup,
  the migration Job with the owner DSN of `ruum-migrate-db`, and `helm upgrade`. The rules of
  "Deploy rules" are unchanged.
- **The outcome** goes back into the request: the annotation `ruum.space/phase` (`Running`,
  `Succeeded`, `Failed`, `Refused`), `data.status` and `data.log` (the last 300 lines of the
  output, without the migration Job's log, credentials in URLs masked). `deploy.sh` prints it as
  it comes. The first line names the controller's commit (the chart that ran). The controller
  deletes every request after a week, whatever its phase (a request created with a preset phase,
  or one it cannot claim, is never run), and keeps at most the newest 50 finished ones. The
  ResourceQuota `release-requests` allows 100 ConfigMaps in `ruum-deploy`, so the deployer
  cannot fill etcd; the controller lists the requests in pages. `status.sh` prints only checked
  values of a request (a commit of 40 hex, one of the four phases); anything else shows as `?`.
- **The chart is the controller's.** The chart, the migration Job and the deploy scripts come
  from the controller's image, the commit of the last platform step (the annotation
  `ruum.space/commit` on `ruum-deploy/deploy-controller`). `deploy.sh` reads that commit and warns
  when the target's `charts/ruum` or deploy scripts differ from it: a chart change on main reaches
  staging only with the platform step from a checkout that has it, then a deploy.
- **The checks of the rights**: `ops/rbac-check.sh static` reads every binding of the deployer in
  the rendered manifests (CI, `tests/run.sh`: each way back to a Secret is refused: a pod, a Job,
  a Deployment patch, `pods/log`, exec, an ephemeral container, a token, a Secret by name, a
  wildcard, a ClusterRole, a group binding); `ops/rbac-check.sh live` asks the API server
  (`kubectl auth can-i --as`) for the deployer in every namespace and for the controller, and
  the platform step stops when an answer is wrong.
- **What the controller can reach**: what the deployer could before (it creates the pods and
  Jobs in `ruum` and `edge`, so through them it reaches the Secrets there). It runs only code
  from an image of main, installed by digest by the admin, and takes nothing from a request but
  the checked fields. Only the admin can change its Deployment, ServiceAccount or Roles.
- **What the deployer can still choose**: which signed commit of main at or after the O-25 floor
  runs: a newer one, or the previous release with `--rollback`. It cannot run anything else.
  It cannot go back past the previous release (Kimi review of O-34, finding 1): an older image
  ignores newer migrations that it does not know (`set_ignore_missing`), so the migration Job does
  not stop a release that reads a dropped column. The controller refuses `rollback: older`, so
  this stays an admin decision until backlog O-37 (`schema_floor`), also when the deployer
  kubeconfig is with an agent or CI.
- **A stop of the controller** (the platform step, a node drain): it takes no new request after
  SIGTERM and lets a running deploy finish (`terminationGracePeriodSeconds` 3660, longer than one
  deploy). The platform step (`k8s/apply.sh`) stops before any change while a deploy runs (the
  deploy lock is held, or a request is `Running`); the admin can pass
  `secrets-push.sh platform --ignore-running-deploy`.

### Observer (troubleshooting)

The separate ServiceAccount `ruum/observer` can get, list and watch pods, events (both APIs),
deployments, daemonsets, replicasets and jobs in `ruum`, `edge` and `ruum-deploy`. It can read pod
logs. It never sees Kubernetes Secrets or ConfigMaps, cannot exec, attach or port-forward, and
cannot add ephemeral containers, mint tokens or write anything. The platform step writes its
own `observer.kubeconfig` beside `deployer.kubeconfig` in `~/.config/ruum/staging` on the Mac
(or `RUUM_STAGING_WORK`), mode 0600. Use that file for troubleshooting through the admin tunnel;
keep it separate from deploy credentials. `ops/rbac-check.sh` checks its bindings and rules
statically and its permissions on the live cluster after each platform step.

### Going back past the previous release (the admin)

The controller refuses a request with `rollback: older`, and `deploy.sh --rollback-older` stops
on the Mac with the warning. Only the admin, with the admin kubeconfig, can run such a rollback:

1. Check every migration since the target (`git diff --name-only <target> <live> -- migrations/`)
   for a contract step (a dropped or renamed column or table) that the target still uses. If
   there is one, do not go back: revert on main and deploy the revert.
2. Read the two digests that `deploy.sh` printed for the target (or `ghcr.io/nemecec/ruum-server:<target>`
   and `ruum-edge:<target>` resolved by digest) and the live commit (`status.sh`).
3. Run `ops/deploy-chart.sh` in the controller's pod, with its ServiceAccount, the deploy lock and
   every check of the script (the tag, the provenance with `cosign`, the live release):
   `deploy/staging/tool.sh kubectl -n ruum-deploy exec deploy/deploy-controller -- env RUUM_DEPLOY_IN_CLUSTER=1 bash /opt/ruum/deploy/staging/ops/deploy-chart.sh <target> <server@sha256:...> <edge@sha256:...> --expect-deployed <live> --rollback-older --backup`.
   It prints the warning again. No request is written, so `status.sh` shows no request for it.
4. `bash deploy/staging/ops/smoke.sh`.

### Moving live staging to the deploy controller (once, the orchestrator)

The order keeps the live release serving at every step. Until step 6 passes, the deployer
kubeconfig stays on the person's Mac only (decision 0008-2).

1. Merge to main. Wait for `images.yml` to publish `ghcr.io/nemecec/ruum-deploy-controller:<sha>`
   (`gh run list -R nemecec/ruum-server --workflow images.yml --branch main`).
2. The package is new at GHCR: check that it is public, like `ruum-server` and `ruum-edge`
   (`curl -s "https://ghcr.io/token?scope=repository:nemecec/ruum-deploy-controller:pull&service=ghcr.io"`
   gives a token, and the manifest of the tag answers 200). If not, the person sets it to
   public once (GitHub, the package's settings, "Change visibility"). The cluster pulls it
   without credentials.
3. On the Mac, in the main checkout: `git pull` (HEAD is now the merge commit, which has the
   controller image).
4. `RUUM_STAGING_APPLY=yes deploy/staging/secrets-push.sh platform`: the platform step. It
   checks the controller image's provenance, adds the namespace `ruum-deploy`, the controller,
   its Roles, and the deployer's new Roles (the old deployer rights in `ruum`, `edge` and `data`
   go), waits for the controller, and runs `ops/rbac-check.sh live`. It stops when any answer is
   wrong. The live release keeps serving: nothing of `charts/ruum` changes in this step.
5. Check: `deploy/staging/status.sh` shows the controller `Running`.
6. `deploy/staging/deploy.sh <sha of main>`: the first deploy through the controller (a
   redeploy of the live commit is fine). It prints the controller's checks and output and ends
   with `deployed <sha>`, then the smoke checks.
7. Only now may the deployer kubeconfig leave the Mac (an agent or CI), if the person wants that.

Going back: run the platform step from a checkout before this change. It restores the old
deployer rights (Pods, Jobs, Leases, Backups). The controller's ServiceAccount, Roles,
RoleBindings, ResourceQuota and Deployment are templates of the `platform` Helm release, so
`helmfile sync` of that checkout deletes them. Only the namespace `ruum-deploy` (from
`k8s/manifests/namespaces.yaml`, which `kubectl apply` never prunes) and the request ConfigMaps
in it stay; delete them with `deploy/staging/tool.sh kubectl delete namespace ruum-deploy`.
`deploy.sh` of that checkout runs `deploy-chart.sh` on the Mac again. The deployer kubeconfig is
then operator-only again (README S-4 of that checkout).

### Moving an existing staging to the two database roles (once)

The deploy after this change runs the migration Job with `ruum/ruum-migrate-db`, so the platform
step must run first. The order keeps the live release serving at every step:

1. Merge to main. On the Mac, in the main checkout: `git pull`.
2. `deploy/staging/secrets-init.sh`: adds `STG_PG_RUUM_APP_PASSWORD` to `secrets.env` (prints
   the name only).
3. `RUUM_STAGING_APPLY=yes deploy/staging/secrets-push.sh platform`: the platform step. It adds
   the role `ruum_app` (CNPG), its grants, the Secret `ruum/ruum-migrate-db`, the deployer's
   Lease rights, and then switches `ruum/ruum-server-env` to `ruum_app`'s DSN. The live pods keep
   their environment until they restart; a pod that restarts now logs in as `ruum_app`, which has
   its rights already (the live release does no DDL while it serves).
4. Check: `deploy/staging/tool.sh kubectl -n data exec pg-1 -c postgres -- psql -d ruum -tAc
   "select has_table_privilege('ruum_app', '_sqlx_migrations', 'INSERT')"` prints `f` (use the
   primary's name if it is not `pg-1`).
5. `deploy/staging/deploy.sh <sha of main>`: the migration Job logs in as `ruum`, restricts
   `ruum_app` again, and the rollout starts pods with `ruum_app`'s DSN. This deploy records
   `previous` in `ruum-release` for the first time.
6. `deploy/staging/status.sh --checks` and the smoke checks (deploy.sh runs them).

Going back: a rollback after step 5 to the release before it uses this checkout's chart, so its
migration Job also logs in as `ruum` and its pods as `ruum_app`; it needs `--rollback-older` this
once (no `previous` was recorded before step 5), which the admin runs ("Going back past the
previous release"). To undo the role split itself, run the platform
step from a checkout before this change: it writes the owner's DSN back into `ruum-server-env`
(`ruum_app` and its grants stay, unused); then restart ruum-server
(`deploy/staging/tool.sh kubectl -n ruum rollout restart deployment/ruum-server`).

## Capacity (N-1)

The sum of the memory requests must fit on two nodes, or a node upgrade hangs. Requests in the
charts: Postgres 2 x 512 MiB, Zitadel 384 MiB, ruum-server 2 x 200 MiB, SpiceDB 128 MiB,
operators about 300 MiB, NATS 64 MiB, edge 3 x 64 MiB, deploy controller 64 MiB, Mailpit 32 MiB,
the app gateway 2 x 64 MiB and the egress proxy 32 MiB (card I-6): about 2.8 GiB plus the system
pods (sandboxes come on top, each with the runtime class overhead of 64 MiB),
against about 6.5 GiB allocatable on two cx23. `ops/check-capacity.sh` measures it on the live
cluster; run it in the first week with one node cordoned. If it does not fit, set
`NODE_COUNT=4` before the first upgrade.

## The app runtime (card I-6)

Milestone M2 on staging: apps are served at `<app>.<tenant slug>.apps.staging.ruumapps.net`
(previews `<short id>--<app>.<tenant slug>.apps.staging.ruumapps.net`).

- **gVisor on the nodes.** `talos/schematic.yaml` adds the Talos system extension
  `siderolabs/gvisor`; `TALOS_SCHEMATIC` in `versions.env` is that file's SHA-256 (Image
  Factory's ID; `tests/run.sh` checks it), and the installer image and the snapshot follow it.
  Running nodes get it only through `ops/upgrade-talos.sh` (one node at a time; a node is
  current when it runs `TALOS_VERSION` with that schematic and every extension of the file).
  New nodes boot the snapshot labelled `schematic=<first 32 hex>`; `tofu` never replaces a
  running node for a new snapshot. The runtime class `gvisor` (handler `runsc`,
  `k8s/manifests/runtimeclass.yaml`) is applied by the platform step only after
  `ops/gvisor-check.sh --class-only` found the extension on every node; at its end the step
  runs a pod of that class and a control pod on every node (`ops/gvisor-check.sh`): the control
  must see the node's kernel and the gVisor pod must not.
- **The gateway and the egress proxy** (`charts/ruum` `gateway.yaml`, `egress.yaml`) run from
  `ghcr.io/nemecec/ruum-gateway` and `ruum-egress`, built, scanned and attested by `images.yml`
  with the other images. `ops/deploy-chart.sh` takes them from the tag `<commit>` and checks
  their provenance and their revision label like the server's. One image without the other
  stops the deploy, and so does a registry or network error of either lookup. A commit without
  both images (older than I-6, or the packages are private) deploys without the app runtime only
  when the live release has none, or on `deploy.sh --rollback`: otherwise helm would delete the
  running gateway and egress proxy (review of I-6, 2). The gateway: two replicas,
  `RUUM_GATEWAY_*` settings from the chart, its database role `ruum_gateway` (it may only execute
  `ruum_app_host_lookup`; `k8s/sql/ruum-gateway-grants.sql` fails the platform step when it can
  do more) and the user-token key as a file (`STG_GATEWAY_TOKEN_KEY`). It holds **no object
  store key** (decision 0008-4): ruum-server writes static app files into a bucket of their own,
  `ruum-staging-static-768usm` (`tofu/cluster`, `RUUM_STATIC_S3_BUCKET`), and for each request
  signs a link that reads one file object for one minute; the gateway fetches the bytes with it
  on 443. ruum-server signs only for a deployment that serves now and is static. Hetzner keys
  cannot be limited to one bucket or to reads, so a read-only gateway key would need Garage as
  well; signed links need no extra service. ruum-server copies site objects that an earlier
  release wrote into the blob bucket over to the static bucket (each sweep, every 5 minutes; until
  a site is copied, its links point into the blob bucket). The copies in the blob bucket stay in
  this release, so `deploy.sh --rollback` to the earlier release finds every site it wrote; a
  later release deletes them (backlog I6-3D-3). The static bucket has no copy job:
  every site can be built again from its deployment archive in the blob bucket. The egress proxy: one replica, CONNECT on 443, `EGRESS_DENY` from the Hetzner API
  (the cluster ranges and every public address of staging and of the build server, ConfigMap
  `ruum/ruum-egress-deny`; the platform step stops when an API answer fails or misses a node, the
  load balancer or the build server, and restarts the proxy when the list changed), its control
  socket inside the pod.
- **Network policy** (`charts/platform` `networkpolicies.yaml`): nothing reaches the gateway but
  the edge (8494); the gateway reaches only ruum-server's internal listener (8493), Postgres,
  the sandboxes, DNS and public addresses on 443 (the static bucket, through the signed links); sandboxes in `ruum-apps` get
  ingress only from the gateway and egress only to the egress proxy (3128: no DNS, no other
  pod, no metadata address); only `ruum-apps` reaches the egress proxy, which reaches DNS and
  public addresses on 443. "Public" excludes the private and special-use ranges that
  `crates/ruum-egress/src/addr.rs` refuses (IPv6: global unicast `2000::/3` only).
  `status.sh --netpol` probes each path, a sandbox of the runtime class gvisor included.
- **The edge and DNS.** The site `*.*.apps.staging.ruumapps.net` goes to the gateway with
  on-demand TLS and no HSTS; ruum-server's `ask` endpoint answers 200 only for a host that can
  serve (`ruum_api::internal`): the live host of an app with a live deployment that is not in
  the trash, or the preview host whose short ID is a pending, running or ready preview of that
  app. It shares the hourly limit with the content hosts, checks an allowed name again after
  10 minutes, remembers a refused name for a minute and runs at most 8 lookups at a time. `tofu/cluster/dns.tf`: `*.apps.staging` A, AAAA and CAA.
  `RUUM_APPS_DOMAIN` is the same in ruum-server and the gateway (the app addresses, the sign-in
  redirect to an app origin).
- **Smoke checks** (`ops/smoke-apps.sh`, in `status.sh --checks`): DNS, no certificate for an
  unknown app host, and for the hosts in `STG_SMOKE_STATIC_HOST` (and, later,
  `STG_SMOKE_NODE22_HOST`) a certificate, no HSTS, and the gateway's sign-in chain to the
  platform. The page behind the sign-in is the person's browser check.

**Staging apps of different tenants are not separate sites yet** (backlog I6-11). Browsers
group origins into sites by the Public Suffix List. `ruumapps.net` is not on it, so every app
origin under `apps.staging.ruumapps.net` (and the content hosts under
`content.staging.ruumapps.net`) is one site, `ruumapps.net`. One tenant's app can set a cookie
for `.apps.staging.ruumapps.net` or `.ruumapps.net` that other tenants' apps and the content
hosts receive, and per-site reputation (Safe Browsing) crosses tenants. The gateway's
`__Host-` cookies and the platform sign-in are not affected; tenant apps' own cookies are. The
PSL entry of decision 0006 E-1 (`*.ruumapps.net`) is not enough for staging: it makes
`apps.staging.ruumapps.net` one site. Do not put anything on staging apps that needs the
separation.

**Not on staging yet** (backlog I6-2 to I6-6): node22 deploys (the deploy pipeline refuses
`runtime: node22` with `runtime_not_ready`), so node22 apps answer the gateway's 502 after the
sign-in and the node22 smoke check prints BLOCKED; a builder and a registry on staging (the
Docker builder needs a Docker engine; Talos has none); the registration of sandboxes at the
egress proxy (its identity is a registered address, and its control socket is inside the
proxy's pod: backlog I6-2, a panel question); the database pooler, the telemetry listeners, the
key set of user tokens for apps (P-97).

### The runtime driver (backlog I6-1, I6-20, S-89)

ruum-server starts app sandboxes as pods with `RUUM_RUNTIME_DRIVER=kubernetes`
(`crates/ruum-runtime` `src/kube`, the same trait, spec and limits as the Docker driver of the
laptop; its README "Kubernetes driver" lists every API call). **Off on staging** (decision
0008-5): `charts/ruum` sets it only with the app runtime and `runtime.kubernetes=true`, and then
it also needs `runtime.supervisorImage` (by digest) and `runtime.nodeNetworkEncrypted=true`, or
the chart refuses to render.

- **The pod of a run** (`ruum-i-<instance>-<run>` in `ruum-apps`): `runtimeClassName: gvisor`,
  the ServiceAccount `ruum-sandbox` (no rights) with one projected token of the audience
  `ruum-supervisor` (600 s), no host namespace, user 10001, read-only root, `drop: [ALL]`, no
  privilege escalation, `RuntimeDefault` seccomp, CPU and memory limits (requests = limits), a
  memory-backed `/tmp` of the disk limit, no DNS (`/etc/hosts` lines for the routes),
  `restartPolicy: Never`, `activeDeadlineSeconds` for a job. The init container
  `ruum-supervisor` copies the sandbox supervisor into a memory volume, which the app container
  mounts read-only and runs as its first process (`docs/STEP3.md` "The sandbox supervisor"):
  the supervisor fetches the secret values once from the server's supervisor listener and
  redacts the app's output before the kubelet writes it. The driver reads the stored pod back
  and deletes it when any setting differs.
- **The record**: a ConfigMap `ruum-i-<instance>` holds the instance's state (every server
  replica answers for every instance, also after a restart) and its values **sealed** under the
  record key (Secret `ruum/ruum-runtime-record-key`, mounted only into the server pods), with the
  `uid` of the pod a server process made for the run. No Secret in `ruum-apps`.
- **Its rights** (`charts/platform` `runtime-rbac.yaml`): the ServiceAccount `ruum/ruum-server`
  with a Role in `ruum-apps`: pods (create, get, list, delete), `pods/log` (get), ConfigMaps
  (create, get, list, update, delete); and one ClusterRole rule: `tokenreviews` (create). No verb
  on Secrets, no exec, attach or port-forward, no token request, no RBAC change. The server pod
  mounts the account's token only as a projected volume (one hour) for the API server.
  `ops/rbac-check.sh live` checks each right and each refusal (also every Secret verb, a no-op
  patch of a Secret, and that `ruum-apps/ruum-sandbox` has no right).
- **Admission** (`k8s/manifests/apps-admission.yaml`): every pod in `ruum-apps` must have the
  supervisor's shape (gVisor, the account `ruum-sandbox`, only `emptyDir` volumes and the
  supervisor's projected token, no `valueFrom` or `envFrom`, at most the init container
  `ruum-supervisor`, a read-only supervisor volume, the termination message from its file), and
  no Secret is admitted in `ruum-apps`. `ops/gvisor-check.sh` proves the rules with server-side
  dry runs and a positive control.
- **The supervisor listener**: port 8091 of each server pod, the Service `ruum/ruum-supervisor`;
  `charts/platform` `networkpolicies.yaml` lets only `ruum-apps` reach it, and the sandboxes reach
  only it and the egress proxy.
- **Not yet**: no registry (`runtime.registry` empty: every app image is `image_not_found`), no
  egress registration (I6-2: the proxy refuses every sandbox, fails closed), no supervisor image
  in `images.yml`, logs are read only by the replica that started the instance, and stdout and
  stderr come as one stream unless the API server has the feature gate
  `PodLogsQuerySplitStreams` (`runtime.splitLogStreams`).

#### Turning the Kubernetes runtime driver on

The binding that S-89 asked for exists (card I6-20), but the driver stays off until the person
decides (decision 0008-5). Each step stops on the first error; the live release keeps serving.

The person:

1. Decide to turn it on (a line in `docs/decisions/0008-person-decisions-2026-11-13.md` item 5
   or a new decision), after reading `docs/STEP3.md` "The sandbox supervisor" (threat model) and
   decision 0006 I.
2. Delete any Secret left in `ruum-apps` by an older release: `deploy/staging/tool.sh kubectl -n
   ruum-apps get secrets` must answer "No resources found"; otherwise `deploy/staging/tool.sh
   kubectl -n ruum-apps delete secret <name>` for each (the new policy does not check existing
   objects).

The orchestrator (cards first, then the platform step):

3. **The supervisor image**: a card adds `crates/ruum-runtime/supervisor/Dockerfile` to
   `.github/workflows/images.yml` (linux/amd64, provenance as the other images, decision
   0008-3). Its digest goes into `runtime.supervisorImage` (`ghcr.io/nemecec/ruum-supervisor@sha256:...`).
4. **Node-to-node encryption** (decision 0006 I point 4): an O card turns on Talos KubeSpan
   (`machine.network.kubespan.enabled: true` in `talos/patches/cluster.yaml`, applied through the
   admin tunnel) or the CNI's WireGuard. Check on every node: `deploy/staging/tool.sh talosctl -n
   <node ip> get kubespanpeerstatuses` lists every other node with state `up`. Only then may
   `runtime.nodeNetworkEncrypted: true` be set.
5. **I620-9** (the shared launch authority across server replicas, `docs/backlog/2026-11-26-i620.md`):
   built and merged before the first real tenant's secret runs on Kubernetes. Staging runs two
   server replicas behind the Service `ruum-supervisor`, so the rollback residual of
   `docs/STEP3.md` "Rollback" applies there. Until then only test values run on the driver.
   Also I620-1 to I620-3 (the published image, encrypted node traffic, the gVisor proofs) and
   I620-12 (a bound on the TokenReviews of all sandboxes together, and a cheap check before
   one). Done before this gate (Kimi run 16 re-check of I6-20): T-85 (a same-process reseal
   race refused a legitimate rotation; `docs/backlog/2026-11-26-run16.md`) and I620-13 (the
   supervisor listener's own connection caps and header timeout).
6. **I6-19**: the kubelet's `podPidsLimit` in the Talos machine config (the hard PID bound of a
   sandbox), with its live fork check in `ops/gvisor-check.sh`.
7. **The record key**: `deploy/staging/secrets-init.sh` (adds `STG_RUUM_RUNTIME_RECORD_KEY`, 64
   hexadecimal characters; a name that is present is never changed), then
   `RUUM_STAGING_APPLY=yes deploy/staging/secrets-push.sh platform`. This step also applies the
   new admission policy, the ClusterRole `ruum-runtime-tokenreview`, the ServiceAccount
   `ruum-apps/ruum-sandbox` and the network policies, and runs `ops/rbac-check.sh live`.
8. **Live checks** before the switch:
   - `deploy/staging/tool.sh bash deploy/staging/ops/gvisor-check.sh`: every line `PASS`, among
     them "ruum-apps refuses a pod that mounts a Secret", "... with a projected token of the
     API's audience", "... with a writable supervisor volume", "a restricted gVisor pod in the
     shape of the sandbox supervisor passes the admission rules (the positive control)" and
     "ruum-apps refuses a Secret named for an instance (Opaque)".
   - **The pod read-back** (review of I6-20): the same run ends with "the API server stores the
     driver's sample pod as committed, ...". The first time it fails: it writes the API server's
     answer to `~/.config/ruum/staging/sandbox-sample-pod.stored.json`. Copy that file to
     `deploy/staging/k8s/sandbox-sample-pod.stored.json` in a card, and CI's test
     `the_api_servers_answer_to_the_sample_pod_passes_the_read_back` (crates/ruum-runtime
     `tests/kube.rs`) must pass with it. A default that the driver's check does not know fails
     that test: fix `pod::verify` first (allow it with its exact value only). Then run the check
     again: every line `PASS`. The driver is not turned on before.
   - `deploy/staging/tool.sh bash deploy/staging/ops/rbac-check.sh live` ends with "... the
     runtime driver has its sandbox rights in ruum-apps and TokenReviews only and no verb on
     Secrets ...; the sandbox account has none".
   - `deploy/staging/tool.sh kubectl -n ruum get secret ruum-runtime-record-key -o name` answers
     the Secret (never print its data).
9. **The switch**: in a change of `deploy/staging/k8s/values` (or the chart values),
   `runtime.kubernetes: true`, `runtime.supervisorImage: <digest>`,
   `runtime.nodeNetworkEncrypted: true`; then `deploy/staging/deploy.sh <sha of main>`.
10. **Checks after the deploy**:
   - `deploy/staging/tool.sh kubectl -n ruum logs deploy/ruum-server | grep -E 'runtime driver|supervisor listener|the sweep'`:
     per pod one line `runtime driver: Kubernetes (backlog I6-1)` and one `supervisor listener
     (sandboxes only) listening`, the expected warnings (no egress control listener, no
     registry), and no `the sweep cannot list the instance records`.
   - `deploy/staging/tool.sh kubectl -n ruum get service ruum-supervisor` shows port 8091.
   - `deploy/staging/status.sh --checks`: as before; the node22 line stays `BLOCKED` (no builder,
     no registry).
11. **The first sandbox** (once a registry and node22 deploys exist): start one app whose start
    command prints a known test value of a secret; then the admin checks, on the pod:
    `kubectl -n ruum-apps logs <pod>` shows `<hidden secret>` and not the value (the node's CRI
    file holds the same lines); `kubectl -n ruum-apps exec <pod> -c app -- sh -c 'echo x >
    /proc/1/fd/1'` fails with "Permission denied" (the supervisor is non-dumpable under
    gVisor); `kubectl -n ruum-apps exec <pod> -c app -- sh -c 'kill -9 1; echo $?'` leaves the
    pod running (PID 1 ignores it); and the server logs show one launch per pod and no `refused`
    line for it. A pod made by hand as a copy of it gets no launch (`a supervisor request was
    refused` in the server log, exit 125).

Going back: `runtime.kubernetes: false` and a deploy. The ServiceAccounts, the Role, the
ClusterRole, the admission policy and the record key stay; they are harmless (nothing else
makes pods in `ruum-apps`).

**What the node22 smoke check still needs**, in order: node22 deployments in the deploy pipeline
(the build, image and start stages, and the spec maker of the sandbox directory); a builder that
runs on Kubernetes (build jobs as gVisor pods) and a registry the nodes pull from
(`runtime.registry`, with the containerd mirror of the Talos machine config); the egress control
path (I6-2) for apps that call out. Then the person deploys one node22 app and puts its host into
`secrets.env` as `STG_SMOKE_NODE22_HOST`, and `RUUM_STAGING_SMOKE_NODE22_READY=1
deploy/staging/status.sh --checks` must show for that host: its own certificate, no HSTS, no
Server header, and the gateway's sign-in chain to `https://staging.ruum.space/v1/auth/login` (or
200 for a public app). The page itself is the person's browser check.

### The person, once

1. GitHub, after the first `images.yml` run on main that publishes them: the packages
   `ruum-gateway` and `ruum-egress` (Your profile, Packages, the package, Package settings,
   "Change visibility", Public), like `ruum-server`. Until then `deploy-chart.sh` finds no
   image and deploys without the app runtime.
2. Nothing for static apps (decision 0008-4): the gateway gets no key. If `secrets.env` still has
   `STG_GATEWAY_S3_ACCESS_KEY` and `STG_GATEWAY_S3_SECRET_KEY` from an earlier release, delete
   the two lines, and delete that credential in the Hetzner Console (project of staging, Object
   Storage, "S3 credentials", the one with the description `ruum-staging-gateway`, Delete). No
   script sends it any more; `k8s/apply.sh` deletes the Secret `ruum/ruum-gateway-s3`.
3. After the deploy: sign in at `https://staging.ruum.space`,
   deploy one static app over MCP or the web UI and publish it, put its host into `secrets.env`
   as `STG_SMOKE_STATIC_HOST`, and open it in the browser (a certificate of the configured CA,
   the page after the app sign-in).

### The orchestrator: from main to the app runtime

Each step stops on the first error; the live release keeps serving through all of them.

1. `deploy/staging/up.sh` (read-only plan). Expected: the persistent root unchanged; the cluster
   root creates `hcloud_zone_rrset.apps_a["*.apps.staging"]`, `apps_aaaa["*.apps.staging"]`,
   `app_origins_caa` and the static bucket `minio_s3_bucket.static` (decision 0008-4), and changes `talos_machine_configuration_apply.node["stg-1"]` to `["stg-3"]`
   in place (the installer image); Talos access `tunnel`; nothing else; price unchanged.
2. `deploy/staging/tool.sh bash deploy/staging/ops/upgrade-talos.sh --check` shows each node's
   version, schematic and extensions (read-only). Then
   `deploy/staging/tool.sh bash deploy/staging/ops/upgrade-talos.sh`: one node at a time, through
   the admin tunnel (capacity check, etcd, pods, CNPG switchover, `talosctl upgrade` with the new
   installer image, wait, the schematic and gvisor checked before the next node). About 10
   minutes per node. `--check` again: all current.
3. `RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply --infra-only`: uploads the snapshot of the
   new schematic (a temporary server for about 10 minutes), applies the three DNS records, the
   static bucket (before any server of this release starts, which writes static sites there) and
   the machine configs through the admin tunnel (the nodes already run that installer image, so
   nothing reboots).
4. `deploy/staging/secrets-init.sh` (adds `STG_PG_RUUM_GATEWAY_PASSWORD`, `STG_GATEWAY_TOKEN_KEY`,
   `STG_EGRESS_CONTROL_TOKEN`; prints names only), then
   `RUUM_STAGING_APPLY=yes deploy/staging/secrets-push.sh platform`: the extension check, the
   runtime class, `ruum-apps`, the network policies, the role `ruum_gateway` and its grants, the
   Secrets `ruum/ruum-gateway`, `ruum/ruum-egress` (the Secret `ruum/ruum-gateway-s3` of an
   earlier release is deleted), `ruum/ruum-egress-deny`, the new deploy controller (whose chart has the
   gateway), and the gVisor pod check.
5. `deploy/staging/deploy.sh <sha of main>`: the controller resolves the gateway and egress
   images of that commit and rolls out the chart; ruum-server gets `RUUM_APPS_DOMAIN`.
6. `deploy/staging/status.sh --checks` (smoke and smoke-apps) and `deploy/staging/status.sh
   --netpol`.

Going back: `deploy.sh --rollback` to the previous release removes the gateway and the egress
proxy with the chart (the controller's chart renders them only with both images; the previous
commit has none); the older ruum-server ignores `RUUM_APPS_DOMAIN` and gives no certificate to
app hosts. The runtime class, `ruum-apps`, the policies and the Secrets stay (the platform step of an
older checkout removes the policies; the rest is harmless). The nodes keep gVisor.

## Recreate

A recreate makes a new Zitadel instance: new OIDC client ID and IdP IDs (the bootstrap writes them to
`ruum/ruum-oidc`), no users. The person repeats the
sign-in test. Postgres starts a new backup path (`pg-<generation>`, a tofu output), so the kept
backup bucket never blocks archiving; `--restore-from` reads an older path.

## Sign-in test (the person, about 5 minutes)

Agents cannot sign in with a real Google or Microsoft account. When `status.sh --checks` shows
that both providers redirect correctly, the person opens `https://staging.ruum.space` (with the
ZeroSSL CA), signs in with Google, signs out, signs in with Microsoft. Microsoft emails are not
trusted as verified (nOAuth), so Zitadel asks to verify the address once: the code goes by email
to that address, from `no-reply@notify.staging.ruum.space` (see "Email"; check the spam folder). Accounts are
never linked automatically. After the test, staging and its backups (for the retention window)
hold the person's name and email.

## Costs

`up.sh` prints the plan's price from the live `GET /pricing` (gross, VAT included as the API
reports it) and refuses above EUR 50. `status.sh` prices what exists and lists resources without
`env=staging` as orphans. Estimates not in the API: Hetzner Object Storage base fee (EUR 6.19)
and Scaleway (about EUR 1). Scaleway keeps the old versions that a lifecycle expiry leaves in the
backup bucket; they cost cents and go with `destroy.sh --purge-backups`.

## The review items and where they are fixed

| Item | Fix |
|---|---|
| B-1 | No `user_data` (`tofu/cluster/main.tf`); config applied from the Mac (`talos.tf`); `check-plan.sh` refuses user data; `steps.sh apply` checks every node after the apply. |
| B-2 | `firewall_ids` at create, IPv4 and IPv6 sources; `check-plan.sh` refuses a server without it. |
| B-3 | Backup writer key only in the cluster; compliance lock 7 days (`tofu/persistent`); no barman retention; no email permission in the cluster; `up.sh` stops without the key. |
| B-4 | `images.yml`: publish only on a green ci of a push to main in this repository, exact commit, no PR code. |
| B-5 | `RUUM_SIGNUP_ALLOWED_EMAILS` in ruum-server (sign-in and `createTenant`, tests in `signup.rs`); `ask` rate limit. |
| B-6 | `GET /internal/tls-ask` on `RUUM_INTERNAL_BIND` (tests in `internal.rs`); network policy: edge only; on-demand only in the content block. |
| B-7 | `autoLinking` unspecified for every provider; Microsoft `emailVerified: false`; `zitadel-bootstrap.sh check` reports drift. |
| B-8 | Machine-only first instance, PAT 90 days; no password login, no registration; console and admin APIs 403 to every public address; smoke and drift checks. |
| B-9 | Zones are data sources; `check-plan.sh` refuses any `hcloud_zone` resource and any DNS name outside staging. |
| F-4 (code review) | `check-plan.sh` also refuses firewall rules other than inbound tcp 50000, tcp 6443 and ICMP from the build server's /32, a firewall or server delete or replace (only `--allow-delete` for the destroy plan, `--allow-replace` on purpose), firewall attachments, LB targets and server networks outside the plan or on the build server, label selectors other than `env=staging,role=node`, and any object lock other than COMPLIANCE for `BACKUP_LOCK_DAYS`; servers in child modules count. |
| G-1 (code review 2) | `check-plan.sh` also refuses a known server firewall ID that is not the staging firewall of the plan, `apply_to` on a firewall with or without rules (also unknown; only an existing firewall may keep an unchanged `apply_to` that lists the plan's own servers, which Hetzner reports there), firewall rules unknown at plan time, and a primary IP assigned to a server outside the plan or to the build server. |
| G-2 (code review 2) | No longer applies: `install-ops-user.sh` and its `allowed_signers` check are gone with the build-server mode. |
| Mac only (2026-11-11) | `lib.sh`, `steps.sh`, `tools-image.sh`, `tool.sh`: every step on the Mac in the tools container; no ruum-ops, no credential on the build server; `STG_ADMIN_CIDRS` exactly one IPv4 /32 (`check-plan.sh`, the `admin_cidrs` validation in `tofu/cluster`). |
| Talos through the tunnel (backlog O-21) | `tofu/cluster/talos.tf`: endpoints `127.0.0.1:1500N` (variable `talos_endpoints` keeps the state's endpoint of an untouched resource; a precondition per resource); `talos-access.sh`; `steps.sh plan_cluster`, `apply servers`, `talos-access`; `up.sh --apply` phases; `ops/tunnel-setup.sh --before-bootstrap`; `lib.sh` `STG_TUNNEL_NO_KUBE_CHECK`, `STG_TUNNEL_WAIT`, `stg_tunnel_wait_kube`. |
| Load balancer cutover (Kimi re-check of O-53) | `lb-guard.sh check` in `steps.sh apply`: a plan that moves a port of an existing load balancer service to a new NodePort is refused while no Service of the namespace `edge` has that NodePort (deploy the chart first, then apply). A new cluster is not checked. |
| Admin tunnel (2026-11-11) | The firewall source is the build server's /32 from the Hetzner API (no Mac address detection); `tunnel.sh`, `lib.sh stg_tunnel_up`, `tunnel-relay.sh`, `ops/tunnel-setup.sh`, `ops/buildbox-tunnel-user.sh`: an SSH local forward as `ruumtunnel`, restricted to the node API ports; the credentials stay on the Mac. |
| Kimi review (2026-11-11) | `check-plan.sh` refuses every `hcloud_firewall_attachment` change (deletes included), an in-place change of an existing server's `firewall_ids`, a server firewall ID unknown at plan time when the plan creates no firewall, `apply_to` that is cleared or loses a server, and `apply_to` on a replaced firewall. `lib.sh` names the control socket after a fingerprint of the build server, host key, key and forwards, closes other tunnels, and checks every forward before use; `tunnel-setup.sh` always tests a new tunnel. `buildbox-tunnel-user.sh` rolls back the key file and drop-in on any failure. `tunnel-relay.sh` runs the command only when every relay listens. |
| S-1 | kube-network-policies, default deny per namespace, explicit allows; Postgres TLS `verify-full`; no service account token in workloads, except the edge's (Secrets and Leases of `edge` only, for the certificate storage, backlog O-26) and the deploy controller's (`ruum-deploy`, backlog O-34). |
| S-2 | The admin credentials only on the person's Mac (`~/.config/ruum`), never on the build server; namespaced `deployer` ServiceAccount for deploys, which only writes release requests for the deploy controller (backlog O-34). |
| S-3 | One Secret per consumer; `secrets-push.sh` fixed list, refuses forbidden names; tests. |
| S-4 | Certificates, ACME account keys and challenge tokens in Secrets of the `edge` namespace (backlog O-26); only the edge's ServiceAccount `ruum-edge` has a Role on them; no Hetzner key in the edge. **Since backlog O-34 (decision 0008-2) the deployer cannot read a Secret through any path**: it may only create release requests (ConfigMaps in `ruum-deploy`) and read them, `ruum/ruum-release` and the state of the workloads; it creates no Pod, Job, Deployment, DaemonSet or other workload, uses no pod subresource, reads no Secret, gets no token ("The deploy controller"; `ops/rbac-check.sh` static in CI and live after every platform step). Before O-34 it could read every Secret of `ruum` and `edge` through pods it created (Kimi review of O-25, finding 1; backlog O-29 corrected). The deploy controller now holds those rights, in its own namespace, running only an image of main installed by digest. Until the move to the controller is done on live staging ("Moving live staging to the deploy controller"), the deployer kubeconfig stays on the person's Mac. |
| S-5 | rclone crypt for the blob copy; provider-side encryption stated for CNPG; HTTPS endpoints only. |
| S-6 | Generated secrets in `secrets.env` (`secrets-init.sh`); `--restore-from`. |
| S-7 | Deploy by digest; provenance attestation checked; Trivy scans vulnerabilities and secrets in the layers before the push. |
| S-8 | Proxy protocol only from the LB subnet; CAA on both staging names; HSTS without preload; smoke checks for HSTS, 403 and unknown hosts. |
| S-9 | Pod Security `restricted` on every Ruum namespace (Talos default and labels); non-root, read-only root file systems. |
| C-1 | `stg-4` priced at EUR 7.43; `node_count` 3 or 4 only. |
| C-2 | `price.sh` refuses above EUR 50; `status.sh` and `ops/cost-report.sh` price what exists and list orphans. |
| R-1 | The Mac drives and runs (tools container), secrets on stdin. |
| R-2 | `serverName: pg-<generation>`; `--restore-from`. |
| R-3 | Never production Let's Encrypt; certificates not in a destroyed bucket. |
| R-4 | `steps.sh` checks that hcloud-upload-image left no temporary server. |
| D-1 | No health or kubeconfig data source in OpenTofu; `-refresh=false` fallback. |
| D-2 | `destroy.sh --plan` compares the state with the labelled resources; `--sweep`. |
| D-3 | Three lists in `--plan`. |
| D-4 | The passphrase is in `secrets.env`; keep a copy in the password manager. |
| U-1 | `ops/upgrade-talos.sh`, `ops/upgrade-k8s.sh`; Renovate on `versions.env`. |
| U-2 | Requests sized for N-1; `ops/check-capacity.sh` before upgrades. |
| U-3 | Expand and contract; on-demand backup before new migrations. |
| U-4 | Helm 4 `--rollback-on-failure`. |
| O-1 | As S-2. |

## Storage (local-path)

Talos v1.14 has no kubelet `extraMounts`, and the kubelet sees `/var/mnt` read-only. The writable
host path of local-path-provisioner is the Talos user volume `local-path`
(`talos/patches/local-path-volume.yaml`): a partition of 20 to 22 GiB on the system disk, which
Talos mounts at `/var/mnt/local-path` and propagates into the kubelet. No kubelet setting is
needed. `k8s/manifests/local-path-storage.yaml` points `DEFAULT_PATH_FOR_NON_LISTED_NODES` at that
path. The same patch caps EPHEMERAL (`/var`: images, etcd, logs) at 14 GiB. Without the cap,
EPHEMERAL takes the whole 40 GB disk and Talos cannot create the user volume; then the helper pod
fails with `mkdir /var/mnt/local-path/: read-only file system` and every local-path PVC stays
`Pending`. `tests/talos-validate.sh` checks both documents in the config of every node.

Talos applies a `VolumeConfig` only when it creates the volume. On a node that was installed
without the cap, EPHEMERAL already fills the disk, and a config apply alone does not shrink it.
Do this one node at a time (no reboot is needed for the apply itself):

1. `up.sh --apply` (the new machine config; in-place, no reboot).
2. Check: `talosctl -n <node> get volumestatus u-local-path` (phase `ready` means done; a missing
   or `waiting` volume means EPHEMERAL has no room).
3. If it is not ready: `talosctl -n <node> reset --graceful --reboot --system-labels-to-wipe EPHEMERAL`.
   The node leaves etcd, wipes `/var` (images, the etcd member, pod data on that node), keeps its
   machine config in STATE, reboots, creates EPHEMERAL (14 GiB) and the user volume, and joins
   etcd again. Wait for `talosctl -n <node> etcd members` to show three members and for the node
   to be `Ready` before the next node.

A new cluster (`destroy.sh` and `up.sh --apply`) gets the right layout on first boot. A pending
PVC binds by itself when a node has the volume: the provisioner retries.

## Known gaps

- One API endpoint (code review F-8): `cluster_endpoint` is the private IP of stg-1, and the
  tunnel port 16443 goes to stg-1's public IP. KubePrism covers the kubelets and pods, but joins
  and the Mac need stg-1. If stg-1 is lost (or reboots in an upgrade), the Kubernetes API through
  the tunnel is down until it is back; `tunnel_forward_args` in `tunnel.sh` picks stg-1.
- A fresh create through the tunnel (backlog O-21, "Admin access") is written and tested
  offline only; it has not run against real nodes yet. The first one should be watched: the
  Talos provider sends the machine config to a node in maintenance mode through
  `127.0.0.1:1500N`, with the node's public address as `node`. On the live nodes the state still
  holds the public address as the endpoint; a node's resource moves to its tunnel endpoint with
  its first machine config change through OpenTofu (for example backlog I-6).
- `ops/buildbox-tunnel-user.sh` turns TCP forwarding off for every user of the build server
  except `ruumtunnel` (`AllowTcpForwarding no` before `Match User ruumtunnel`).
- The provenance check with `cosign` in the tools container (`ops/image-check.sh`) is written for
  the attestation that `actions/attest-build-provenance` pushes to GHCR. Check it on the first
  image that `images.yml` publishes from main.
- If the build server is down, kubectl, talosctl and the steps that need the cluster fail; the
  public endpoints of staging are not affected.
- `k8s/apply.sh` uses `helmfile sync`, not `apply`: `apply` needs the helm-diff plugin, which the
  tools image does not carry.

- The Talos dashboard on the Hetzner server console is on. Talos v1.14 has no kernel argument in
  the machine config (`talos.dashboard.disabled=1` was in `machine.install`); turning it off
  needs an Image Factory schematic with that argument (a new snapshot and installer image).
- kube-network-policies needs the `nfnetlink_queue` kernel module; check after bring-up that a pod
  in `default` cannot reach `pg-rw.data:5432` (stgsec section 5).
- The first `helmfile apply` waits for the CNPG cluster before SpiceDB and Zitadel can start; a
  slow image pull can need a second `up.sh --apply`.
- Scaleway bucket policies that bind the backup key to the two buckets are not set (the key's
  permission sets and the compliance lock are the controls).
- The deploy controller (backlog O-34): a pod killed during a deploy anyway (SIGKILL after the
  grace period, a node lost) kills its `deploy-chart.sh`. The next pod marks the request
  `Failed`, and the deploy lock stays until it expires (30 minutes). If `helm upgrade` was running,
  the Helm release stays `pending-upgrade` (or `pending-rollback`) and the next deploy fails with
  "another operation is in progress". Recovery (admin): `deploy/staging/tool.sh env
  HELM_DRIVER=configmap helm -n ruum history ruum`, then `deploy/staging/tool.sh env
  HELM_DRIVER=configmap helm -n ruum rollback ruum <the last revision with status deployed>`, then
  `deploy.sh` again. The controller uses GitHub's API without a token (60 calls per hour per
  address); a failed call fails the request, and `deploy.sh` can run again. The deployer can
  fill the ResourceQuota of `ruum-deploy` (ConfigMaps without the label, or with a finalizer,
  stay until the admin deletes them); that blocks only its own deploys. The controller's checks
  were run against a throwaway k3s on the build server, not yet on live staging (the move above).
