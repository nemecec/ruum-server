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
- About EUR 38 per month (the priced plan of `up.sh`); EUR 45.6 with a fourth node, the last step
  within EUR 50. `up.sh` refuses a plan above EUR 50.

## Where things run

The scripts run on the laptop, where `~/.config/ruum/secrets.env` is. Each step runs on the build
server over ssh; secrets go to the remote process on stdin as `NAME=value` lines, never on a
command line, never in a file, never printed (`lib.sh`, `remote.sh`).

On the build server, the steps that create, change or delete something run as the user
`ruum-ops` from a root-owned checkout of a commit of main (`/opt/ruum-staging/current`). Only
the person moves it, with `ops/install-ops-user.sh` as root (code review F-2):

- the commit must be on main; with `/etc/ruum-staging/allowed_signers` it must also carry a good
  SSH signature by the person, otherwise the script shows the diff of `deploy/staging` and asks
  for the commit's first 12 characters;
- the laptop reaches ruum-ops with its own key (`~/.config/ruum/staging_ops_ed25519`,
  `RUUM_OPS_KEY`). The key has a forced command, `ops/ssh-entry.sh`, which runs only `remote.sh`
  steps with plain-word arguments from the pinned checkout;
- no sudoers rule lets `ruum` (the build jobs) run anything as ruum-ops or root. The person's
  decision on the existing broad sudo of `ruum` (code review F-1) and the exact proposed
  changes are in `panel/staging-buildbox-hardening.md`. Until `ruum` loses that sudo, the
  separation does not protect the credentials.

Read-only steps (plans, `destroy.sh --plan`) can also run as `ruum` from a synced copy:
`RUUM_STAGING_REMOTE=direct`. Plan JSON is scrubbed in a pipe (`plan-scrub.jq`: no variables,
no prior state, no sensitive or Talos values) before it is written, so no plan file on the
build server holds the cluster secrets (code review F-5). The binary plan files are encrypted
by OpenTofu with the state passphrase.

OpenTofu state: `ruum-staging-tfstate` at Scaleway (kept on destroy), encrypted by OpenTofu with
`TOFU_STATE_PASSPHRASE` before it leaves the build server. Keep a copy of the passphrase in the
password manager: without it the state cannot be read, and only `destroy.sh --sweep` can remove
staging.

## Commands

| Command | What it does |
|---|---|
| `deploy/staging/up.sh [--out FILE]` | The priced plan: `tofu plan` of both roots, `check-plan.sh`, `price.sh`, and the platform preflight. Changes nothing. |
| `RUUM_STAGING_APPLY=yes deploy/staging/bootstrap-state.sh` | Creates the state bucket once. |
| `RUUM_STAGING_APPLY=yes deploy/staging/up.sh --apply` | Generates the missing `STG_*` secrets (`secrets-init.sh`), uploads the Talos snapshot if missing, plans, checks, prices, applies the exact plan, checks that every node has empty user data, writes the kubeconfig, installs the platform, and configures Zitadel. |
| `... up.sh --apply --restore-from <generation>` | The same, with Postgres restored from an older backup path (disaster-recovery test). |
| `deploy/staging/deploy.sh <sha>` | Deploys a green commit of main by digest, after the provenance check (on the laptop with `gh`, and again on the build server with `cosign`); `--rollback` to go back on purpose; `--acme zerossl` to switch the CA. |
| `deploy/staging/status.sh [--checks]` | What exists, its price, orphans, nodes, pods, backups, deployed commit; with `--checks` also the smoke checks and Zitadel drift. |
| `deploy/staging/destroy.sh --plan` | Three lists (removed, kept on purpose, never touched) and the state compared with the labelled resources. Read-only. |
| `deploy/staging/destroy.sh` | The same, then asks you to type `destroy ruum-staging`. |
| `deploy/staging/destroy.sh --sweep` | When the state is lost: deletes by label `env=staging`, only the staging names (nodes, their IPs, LB, network, firewall, placement group, and the rrsets of `rrsets.json`), never the build server or the zones. |
| `deploy/staging/tests/run.sh` | The tests of this folder (also CI job `staging`). |
| `ops/upgrade-talos.sh`, `ops/upgrade-k8s.sh` | Upgrades, one node at a time, after a version change in `versions.env`. |
| `ops/restore-test.sh` | Monthly restore test into a temporary cluster. |
| `ops/check-capacity.sh` | The N-1 sizing rule (below). |
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
3. **Zone delete protection.** Hetzner console, DNS: turn on delete protection for `ruum.space`
   and `ruumapps.net`. The code never manages the zones, but the token could delete them.
4. **ruum-ops.** On the laptop: `ssh-keygen -t ed25519 -f ~/.config/ruum/staging_ops_ed25519`.
   On the build server, as the person: `sudo bash install-ops-user.sh <commit of main>
   staging_ops_ed25519.pub` (copy the script from that commit and read it first). Repeat it to
   move ruum-ops to a newer commit. See `panel/staging-buildbox-hardening.md` for the sudo
   changes of user `ruum` that the person decides.
5. Optional: `STG_ACME_EMAIL` for ZeroSSL (the trusted CA for the sign-in test).

## Secrets

The laptop file holds everything; the cluster gets one Secret per consumer and namespace
(`k8s/apply.sh`). `HCLOUD_TOKEN`, the full Scaleway key, the state passphrase and the Stripe keys
never go to the cluster (`secrets-push.sh` refuses them; `check-plan.sh --repo` and the tests
check the files).

| Secret | Holds | Read by |
|---|---|---|
| `data/pg-backup-s3` | backup writer key | CNPG (Barman Cloud plugin) |
| `data/pg-role-{ruum,zitadel,spicedb}` | database passwords | CNPG managed roles |
| `auth/zitadel-masterkey`, `auth/zitadel-db` | master key, DSN | Zitadel |
| `authz/spicedb` | preshared key, DSN | SpiceDB |
| `ruum/ruum-server-env` | DSN, Hetzner S3 key, SpiceDB key, token and content keys, allow-list | ruum-server |
| `ruum/ruum-oidc` | OIDC client ID (made by `ops/zitadel-bootstrap.sh`) | ruum-server |
| `ops/blob-copy` | Hetzner S3 key, backup writer key, rclone crypt password and salt | the nightly copy |
| `nats/nats-auth` | the ruum user's password | NATS |

The Google and Microsoft keys go only into Zitadel's database (through the bootstrap), never into
a Secret. The generated values (`STG_*`) are in `secrets.env` too, so a restore after the loss of
the whole cluster can read Zitadel's data.

Backups: HTTPS in transit. At rest, CNPG backups have only Scaleway's provider-side encryption;
the blob copy is encrypted by rclone crypt with a key that lives off the cluster.

## Edge and certificates

- Caddy, one replica, certificates in an emptyDir, never in a bucket that ruum-server can read.
- The ACME CA is never production Let's Encrypt, so staging never spends production's quota:
  `letsencrypt-staging` during bring-up (untrusted by browsers; HSTS max-age 300 then), `zerossl`
  for the sign-in test (`deploy.sh <sha> --acme zerossl`). CAA allows `letsencrypt.org` and
  `sectigo.com` (ZeroSSL), no wildcard.
- On-demand TLS only in the `*.content.staging.ruumapps.net` block; Caddy asks ruum-server's
  internal listener (port 8493, reachable only from the edge) and fails closed.
- `auth.staging.ruum.space`: sign-in and OIDC paths are public; the console and the admin APIs
  answer 403 except from the build server.

## Deploy rules

- By digest only: the chart accepts only `ghcr.io/nemecec/ruum-{server,edge}@sha256:...`.
  `deploy.sh` checks CI, the digest and the provenance attestation of `images.yml` on
  `refs/heads/main`. `ops/deploy-chart.sh` on the build server validates every argument (commit
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

A recreate makes a new Zitadel instance: new OIDC client ID, no users. The person repeats the
sign-in test. Postgres starts a new backup path (`pg-<generation>`, a tofu output), so the kept
backup bucket never blocks archiving; `--restore-from` reads an older path.

## Sign-in test (the person, about 5 minutes)

Agents cannot sign in with a real Google or Microsoft account. When `status.sh --checks` shows
that both providers redirect correctly, the person opens `https://staging.ruum.space` (with the
ZeroSSL CA), signs in with Google, signs out, signs in with Microsoft. Microsoft emails are not
trusted as verified (nOAuth), so Zitadel asks to verify the address once: the code goes to Mailpit
(`kubectl -n ops port-forward svc/mailpit 8025` on the build server as ruum-ops). Accounts are
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
| B-1 | No `user_data` (`tofu/cluster/main.tf`); config applied from the build server (`talos.tf`); `check-plan.sh` refuses user data; `remote.sh apply` checks every node after the apply. |
| B-2 | `firewall_ids` at create, IPv4 and IPv6 sources; `check-plan.sh` refuses a server without it. |
| B-3 | Backup writer key only in the cluster; compliance lock 7 days (`tofu/persistent`); no barman retention; no email permission in the cluster; `up.sh` stops without the key. |
| B-4 | `images.yml`: publish only on a green ci of a push to main in this repository, exact commit, no PR code. |
| B-5 | `RUUM_SIGNUP_ALLOWED_EMAILS` in ruum-server (sign-in and `createTenant`, tests in `signup.rs`); `ask` rate limit. |
| B-6 | `GET /internal/tls-ask` on `RUUM_INTERNAL_BIND` (tests in `internal.rs`); network policy: edge only; on-demand only in the content block. |
| B-7 | `autoLinking` unspecified for every provider; Microsoft `emailVerified: false`; `zitadel-bootstrap.sh check` reports drift. |
| B-8 | Machine-only first instance, PAT 90 days; no password login, no registration; console and admin APIs 403 except from the build server; smoke and drift checks. |
| B-9 | Zones are data sources; `check-plan.sh` refuses any `hcloud_zone` resource and any DNS name outside staging. |
| F-4 (code review) | `check-plan.sh` also refuses firewall rules other than inbound tcp 50000, tcp 6443 and ICMP from the build server's addresses, a firewall or server delete or replace (only `--allow-delete` for the destroy plan, `--allow-replace` on purpose), firewall attachments, LB targets and server networks outside the plan or on the build server, label selectors other than `env=staging,role=node`, and any object lock other than COMPLIANCE for `BACKUP_LOCK_DAYS`; servers in child modules count. |
| S-1 | kube-network-policies, default deny per namespace, explicit allows; Postgres TLS `verify-full`; no service account token in workloads. |
| S-2 | `ruum-ops` user, root-owned checkout of a commit of main that only the person moves, laptop key with a forced command (no sudo rule for `ruum`); namespaced `deployer` ServiceAccount. |
| S-3 | One Secret per consumer; `secrets-push.sh` fixed list, refuses forbidden names; tests. |
| S-4 | Certificates in the edge's emptyDir; no Hetzner key in the edge. |
| S-5 | rclone crypt for the blob copy; provider-side encryption stated for CNPG; HTTPS endpoints only. |
| S-6 | Generated secrets in `secrets.env` (`secrets-init.sh`); `--restore-from`. |
| S-7 | Deploy by digest; provenance attestation checked; Trivy scans vulnerabilities and secrets in the layers before the push. |
| S-8 | Proxy protocol only from the LB subnet; CAA on both staging names; HSTS without preload; smoke checks for HSTS, 403 and unknown hosts. |
| S-9 | Pod Security `restricted` on every Ruum namespace (Talos default and labels); non-root, read-only root file systems. |
| C-1 | `stg-4` priced at EUR 7.43; `node_count` 3 or 4 only. |
| C-2 | `price.sh` refuses above EUR 50; `status.sh` and `ops/cost-report.sh` price what exists and list orphans. |
| R-1 | Laptop drives, build server runs, secrets on stdin. |
| R-2 | `serverName: pg-<generation>`; `--restore-from`. |
| R-3 | Never production Let's Encrypt; certificates not in a destroyed bucket. |
| R-4 | `remote.sh` checks that hcloud-upload-image left no temporary server. |
| D-1 | No health or kubeconfig data source in OpenTofu; `-refresh=false` fallback. |
| D-2 | `destroy.sh --plan` compares the state with the labelled resources; `--sweep`. |
| D-3 | Three lists in `--plan`. |
| D-4 | The passphrase is in `secrets.env`; keep a copy in the password manager. |
| U-1 | `ops/upgrade-talos.sh`, `ops/upgrade-k8s.sh`; Renovate on `versions.env`. |
| U-2 | Requests sized for N-1; `ops/check-capacity.sh` before upgrades. |
| U-3 | Expand and contract; on-demand backup before new migrations. |
| U-4 | Helm 4 `--rollback-on-failure`. |
| O-1 | As S-2. |

## Known gaps

- One API endpoint (code review F-8): `cluster_endpoint` is the private IP of stg-1, and the
  build server's kubeconfig points at the public IP of stg-1. KubePrism covers the kubelets and
  pods, but joins and the build server need stg-1. If stg-1 is lost, point the kubeconfig at
  stg-2 (`write_configs` in `remote.sh` reads `node_ipv4["stg-1"]`; use `stg-2` there) until stg-1
  is back.
- The provenance check with `cosign` on the build server (`ops/image-check.sh`) is written for the
  attestation that `actions/attest-build-provenance` pushes to GHCR. Check it on the first image
  that `images.yml` publishes from main.

- kube-network-policies needs the `nfnetlink_queue` kernel module; check after bring-up that a pod
  in `default` cannot reach `pg-rw.data:5432` (stgsec section 5).
- The first `helmfile apply` waits for the CNPG cluster before SpiceDB and Zitadel can start; a
  slow image pull can need a second `up.sh --apply`.
- Scaleway bucket policies that bind the backup key to the two buckets are not set (the key's
  permission sets and the compliance lock are the controls).
