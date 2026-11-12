# Staging as code

Staging of Ruum on Hetzner (`hel1`), as decided in decision 0007 (budget EUR 30-50 per month). The
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
  user `ruumtunnel`, around the steps that need the cluster (`up.sh --apply` after OpenTofu,
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
- **OpenTofu and Talos.** The Talos provider of `tofu/cluster` talks to the nodes' public
  addresses, which only the build server can reach. `up.sh --apply` refuses a plan that creates
  or changes a machine config or the bootstrap (the plan prints a warning). See "Known gaps".

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
| `RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply` | Generates the missing `STG_*` secrets (`secrets-init.sh`), uploads the Talos snapshot if missing, plans, checks, prices, applies the exact plan, checks that every node has empty user data, writes the talosconfig, opens the admin tunnel, writes the kubeconfig, installs the platform, and configures Zitadel. |
| `... up.sh --apply --restore-from <generation>` | The same, with Postgres restored from an older backup path (disaster-recovery test). |
| `RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply --infra-only` | Only the plan, the checks, the apply, the talosconfig and the kubeconfig. |
| `deploy/staging/ops/tunnel-setup.sh` | The admin tunnel: the key (only if missing), the `ruumtunnel` user on the build server, the host key, a test ("Admin access"). |
| `deploy/staging/deploy.sh <sha>` | Deploys a green commit of main by digest, after the provenance check (with `gh` on the Mac, and again with `cosign` in the tools container); `--rollback` to go back on purpose; `--acme zerossl` to switch the CA. |
| `deploy/staging/status.sh [--checks]` | What exists, its price, orphans, nodes, pods, backups, deployed commit; with `--checks` also the smoke checks and Zitadel drift. |
| `deploy/staging/status.sh --netpol` | The network policy probe (`ops/netpol-probe.sh`): an unknown pod reaches no platform port; ruum-server reaches only Postgres, SpiceDB HTTP, NATS and Mailpit; the edge reaches only ruum-server and Zitadel. Not read-only (a temporary namespace, ephemeral containers). |
| `deploy/staging/destroy.sh --plan` | Three lists (removed, kept on purpose, never touched) and the state compared with the labelled resources. Read-only. |
| `deploy/staging/destroy.sh` | The same, then asks you to type `destroy ruum-staging`. |
| `deploy/staging/destroy.sh --sweep` | When the state is lost: deletes by label `env=staging`, only the staging names (nodes, their IPs, LB, network, firewall, placement group, and the rrsets of `rrsets.json`), never the build server or the zones. |
| `deploy/staging/tests/run.sh` | The tests of this folder (also CI job `staging`). On the Mac, helm and tofu come from the tools image. |
| `deploy/staging/tests/server-env.sh` | The guard against drift between the server and the chart: every `RUUM_*` setting of the chart and of `ruum/ruum-server-env` is one the server reads, and every setting it needs in production is set. The list is `crates/ruum-server/env-names.txt`, checked against the code. Change the list when the server gets a new setting. |
| `deploy/staging/tool.sh COMMAND...` | One command in the tools container (kubectl, talosctl, helm, the `ops/` scripts below). `--port N` publishes a port on `127.0.0.1` of the Mac. |
| `deploy/staging/tools-image.sh [build \| clean]` | Builds or removes the tools image (the steps build it when it is missing). |
| `tool.sh bash deploy/staging/ops/upgrade-talos.sh`, `... ops/upgrade-k8s.sh` | Upgrades, one node at a time, after a version change in `versions.env`. |
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
| `data/pg-role-{ruum,zitadel,spicedb}` | database passwords | CNPG managed roles |
| `auth/zitadel-masterkey`, `auth/zitadel-db` | master key, DSN | Zitadel |
| `auth/zitadel-smtp` | SMTP login at Scaleway: `user` (the project ID, `SCW_DEFAULT_PROJECT_ID`), `password` (`SCW_SMTP_SECRET_KEY`) | Zitadel (setup of a new instance), `ops/zitadel-bootstrap.sh` |
| `authz/spicedb` | preshared key, DSN | SpiceDB |
| `ruum/ruum-server-env` | DSN, Hetzner S3 key, SpiceDB key, token and content keys, allow-list | ruum-server |
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

- Caddy, one replica, certificates in an emptyDir, never in a bucket that ruum-server can read.
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
  `refs/heads/main`. `ops/deploy-chart.sh` in the tools container validates every argument (commit
  40 hex, issuer from a fixed list, a plain email), checks that the tag `<commit>` is that
  digest, verifies the attestation again with `cosign`, and gives helm a values file, never
  `--set` (code review F-3).
- Migrations follow **expand and contract**: a release only adds; a removal comes one release
  later. A failed rollout rolls the image back (`--rollback-on-failure`, Helm 4) but not the
  schema, so the previous image must work on the new schema. `deploy.sh` takes an on-demand CNPG
  backup before a release with new migrations.
- An older commit than the deployed one needs `--rollback`.

## Capacity (N-1)

The sum of the memory requests must fit on two nodes, or a node upgrade hangs. Requests in the
charts: Postgres 2 x 512 MiB, Zitadel 384 MiB, ruum-server 200 MiB, SpiceDB 128 MiB, operators
about 300 MiB, NATS 64 MiB, edge 64 MiB, Mailpit 32 MiB: about 2.2 GiB plus the system pods,
against about 6.5 GiB allocatable on two cx23. `ops/check-capacity.sh` measures it on the live
cluster; run it in the first week with one node cordoned. If it does not fit, set
`NODE_COUNT=4` before the first upgrade.

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
| Admin tunnel (2026-11-11) | The firewall source is the build server's /32 from the Hetzner API (no Mac address detection); `tunnel.sh`, `lib.sh stg_tunnel_up`, `tunnel-relay.sh`, `ops/tunnel-setup.sh`, `ops/buildbox-tunnel-user.sh`: an SSH local forward as `ruumtunnel`, restricted to the node API ports; the credentials stay on the Mac. |
| Kimi review (2026-11-11) | `check-plan.sh` refuses every `hcloud_firewall_attachment` change (deletes included), an in-place change of an existing server's `firewall_ids`, a server firewall ID unknown at plan time when the plan creates no firewall, `apply_to` that is cleared or loses a server, and `apply_to` on a replaced firewall. `lib.sh` names the control socket after a fingerprint of the build server, host key, key and forwards, closes other tunnels, and checks every forward before use; `tunnel-setup.sh` always tests a new tunnel. `buildbox-tunnel-user.sh` rolls back the key file and drop-in on any failure. `tunnel-relay.sh` runs the command only when every relay listens. |
| S-1 | kube-network-policies, default deny per namespace, explicit allows; Postgres TLS `verify-full`; no service account token in workloads. |
| S-2 | The admin credentials only on the person's Mac (`~/.config/ruum`), never on the build server; namespaced `deployer` ServiceAccount for deploys. |
| S-3 | One Secret per consumer; `secrets-push.sh` fixed list, refuses forbidden names; tests. |
| S-4 | Certificates in the edge's emptyDir; no Hetzner key in the edge. |
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
- A fresh create (and any change of the Talos machine config through OpenTofu) is not possible
  from the Mac now: the Talos provider dials the nodes' public addresses, which only the build
  server reaches, and the node addresses are new after a recreate (the `ruumtunnel` key line
  lists them). `up.sh --apply` refuses such a plan before it applies anything. A recreate needs
  either the Talos resources pointed at the tunnel (`endpoint = 127.0.0.1:1500N`, after a
  targeted apply of the servers and `ops/tunnel-setup.sh` for the new addresses) or the build
  server on the staging network; both are follow-ups.
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
