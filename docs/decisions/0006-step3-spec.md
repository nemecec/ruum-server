# 0006: Step 3 specification decisions

Status: `provisional` (design section 18.9). Proposed by the step 3 spec run on branch `s3/spec`
(2026-11-10), reviewed by the architect panel (card C-3: `ruum-server-wt/panel/step3-review-1.md`,
reviewer 1, security, privacy and cost lens; `step3-review-2.md`, reviewer 2, swarm, order, later
steps and developer experience), and revised on branch `s3/spec-3` (2026-11-10) with every
required change of both reviews (R1 to R15, RC-1 to RC-15). Where the reviews disagree, the safer
option is taken; section C names each case. A person can veto any item. The decisions that belong
to the person are the briefs of section E; they are not decided here.

## A. Decisions

### 1. Two new binaries; the internet-facing gateway holds no tenant keys

`crates/ruum-gateway` serves the apps domain (`*.ruumapps.net`) and the report origin paths, not
the core server. Reasons: it answers another registered domain (a bug in it must not touch the
platform origin), it carries all app traffic (it scales and fails alone, design 12), and it holds
the WebSockets. It holds **no tenant keys and no general system access**: only the narrow system
lookups of `docs/STEP3.md` (host, custom domain, partner key, each returning `(tenant_id, id)`),
SpiceDB checks, the user-token signing key and a small internal API of the core; its host cache is
fed by the journal. Everything at `ruum.internal` (the runtime API, the internal gateway for
app-to-app calls, the job scheduler, event delivery) is the second new binary,
`ruum-runtime-api`. The telemetry processor is a role of `ruum-server`. (Open question 1;
reviewer 2's "one binary" is taken for the internal listeners, reviewer 1's condition keeps the
internet-facing part apart.)

### 2. A version of a web app is one live deployment

Design 9.1: "Each version is one deployment. Rollback is restore version 12." A version is
written at **publish** (and at rollback), not for previews, so the release history is
`listVersions` and `restoreVersion` is a rollback. The encoding is `app_release_v1`: the
canonical JSON of the deployment ID, the archive's chunk list and the parsed `app.yaml`, so the
blob GC sees every chunk of every release. Previews are rows of `app_deployments` whose chunk
list keeps their archive alive until the preview expires. `putContent` and uploads keep refusing
typed nodes (decision 0004-6); code enters through `createAppDeployment` from a file node (an
archive) or a folder (the platform packs the head versions of its files and records them; review
RC-1). `AppDeployment.source` names the deployed code; source kinds are an open set (step 4 adds
`build`).

### 3. No privilege escalation through bindings

Design 9.4. At deploy time, for every binding the deployer must hold the binding's role on the
target (for an agent: its user's right intersected with its scope), and for an API binding the
right to grant its scopes (`app_api_scope#grant`: `manage` on the called app, or the app role
named in `granted_by`). Else `createAppDeployment` answers `403 permission_denied` at once (hint
`binding_not_allowed`, the binding in `error.details.binding`; the cheap stages run before the
`202`, review RC-3). `app_bindings.granted_*` records the deployer. An app never gets `manage` on
a node. **In step 3 a file or folder binding is `view`**; `edit` only on a database. App writes to
files come with a later additive change of the runtime API (review RC-6, the deferral option). A
binding to a node of another tenant fails exactly like a missing node. Migrations run as the
platform at deploy time with the deployer's right. At publish the check runs again for the live
roles. Database bindings use the app role kinds of decision 0004-32, built by the step 2 card D-5
(proposed; review RC-11); an app role has `CONNECT` only on its own database, reaches it through a
pooler, and has no `EXECUTE` on `ruum_mask.*` (review R8).

### 4. An agent's publish waits for a person, who decides once

Design 11.1 and decision 39. Until step 5 brings `project.yaml` approval rules, every publish or
rollback by an agent answers `202` with an `AppApproval`; the agent's user or a person with
`manage` on the app decides (`decideAppApproval`, users only), with every right the publish needs
(`edit` on the app, `manage` on **every** database that a migration changes). The decision covers
the telemetry fields and the column classes of the planned columns (`telemetry_fields`, `classes`,
with the step 2 semantics; review RC-2), and it **covers the D-2 migration approvals** of those
migrations, including `write_live` for an agent: the person is asked once. The agent then sends the
same call with `approval_id`: used once, within 15 minutes, only by the agent that asked, for
exactly that deployment, bound to its archive hash, its parsed manifest and the binding targets
resolved at preview time; a publish that resolves anything differently fails (`409`, hint
`approval_stale`; review S5). Previews need no approval. An agent cannot roll back through
`restoreVersion` (`403`, hint `publishAppDeployment`, the deployment in `error.details`). Reading
publish requests needs `edit` (review R14). `getAppApproval` takes `wait`.

### 5. Previews never touch live data, and get nothing that the agent chose

Decision 30 and design 8.5 (review R4). A preview is a principal of its own (the deployment ID;
the live principal is the app node ID) with its own user-token audience
(`app:<app>/preview/<deployment>`). Its database bindings resolve to a branch (`masked`; until the
step 2 card D-3b lands, `schema_only` with seed data), shared by previews with the same `draft`
label. Its other node bindings are capped at `view`: those are reads of live data, allowed
because the deployer can already read them. Its API bindings get only the scopes that the called
API lists in `preview_scopes` (none without the list; the stage `bindings` warns). Generated
secrets get preview values. An entered secret reaches a preview only when a user with `manage`
turned on `preview` for it (`setAppSecret`, stored in `app_secrets.preview`); the manifest field
`secrets.<name>.preview` is gone. Previews keep the egress proxy limits and run no jobs. A preview
is reachable by people with `open_preview` (today `edit`) and by agents with an agent token, and
expires after 7 days without a request; a new preview with the same `draft` supersedes the older.

### 6. Issues are typed nodes

Design 9.7 says "a file of type `issue`". An issue is a typed node (provider `ruum.issue`, content
`contracts/manifests/issue.schema.json` as versions), so its content is validated against its
template like a manifest, counts live in provider state without making a version per
occurrence, and `putContent` cannot write an unvalidated issue. Everything else of 9.7 holds.
Both reviewers agree (open question 10). A one-line design text change follows. Issues do not
federate in the first version of federation (typed nodes are read-only placeholders there).

### 7. Agents never handle secret values in step 3

Design 9.9 allows "an agent with that person's approval" to paste an entered secret. Carrying a
secret through an approval would put the value in an approval record or an agent transcript. In
step 3 `setAppSecret` is users only; agents see names and states (`listAppSecrets`) and tell
their person what is missing. Generated secrets need nobody. Revisit in step 5 with a field on the
approval page that the person fills (open question 9). Stated honestly (review S7): an editor who
publishes code can read entered secrets through that code; the publish approval is the guard for
agents.

### 8. The pseudonym of a known value is for people

`computeAppPseudonym` (design 9.6) is users only and rate limited: an agent could otherwise test
guesses against the logs (the oracle of decision 0004-24). For the same reason app roles have no
`EXECUTE` on `ruum_mask.*` (decision 3).

### 9. No new error codes

Every new refusal uses an existing code with a hint: `permission_denied` (`binding_not_allowed`,
`public_apps_not_allowed`, `custom_domains_not_allowed`, `tenant_admin_required`,
`publishAppDeployment`), `conflict` (`secret_missing`, `raw_copy_gone`,
`database_branches_not_ready`, `approval_stale`, `slug_retired`), `user_required`. Machine-readable
details go into the new optional `Error.error.details` and `AppDeploymentStage.error` (review
RC-3).

### 10. Three credentials that never mix, and tokens open for later steps

The REST API accepts only API tokens (`aud` = the API origin). The runtime API at `ruum.internal`
accepts only workload tokens (`typ: app_workload`) and delegated tokens (`typ: app_delegated`,
`contracts/apps/delegated-token.schema.json`, 5 minutes). Apps verify only user tokens (`typ:
app_user`, `aud: app:<id>` or the preview audience, `env` required, from a key set apart from the
API keys). The gateway accepts an agent token only with the audience `ruumapps`, only on preview
hosts in step 3. A replay across any two fails by `aud`, `typ` and `env`. `exchangeAppUserToken`
refuses a user token whose `aud` or `env` does not match the caller, and an `actor` other than
`user` or `agent` without `delegate` (review R3). The gateway removes its cookies, partner keys
and agent tokens before forwarding and drops an app's `Set-Cookie` for `ruum` names; the internal
gateway removes the caller's workload token (review R2). The user token is open (review RC-4):
`iss` is the instance's origin, unknown claims are ignored, `actor` and `sub` prefixes are open
sets.

### 11. The gallery is part of the platform

Gallery templates (`gallery:<slug>`) ship with the platform release, loaded at start through a
`TemplateSource` from a configured directory, not as nodes of a system tenant, so
`instantiateTemplate` never reads another tenant.

### 12. Approvals of the platform admin are tenant state

`Tenant.app_approval` (read-only through the API) and `tenant_app_settings` hold what a Ruum
platform admin approved. The admin console has its own contract (card C-4, `contracts/admin/`)
before any public app, custom domain or public project (open question 17). The suite fixtures
have both approvals, so the permission rules test the positive case.

### 13. Telemetry follows the classes of 8.5 at ingestion, from authenticated sources

`secret` is never stored, `personal` is a keyed pseudonym with the tenant key, unclassified is
`personal`, personal metric labels are dropped. The processor accepts records only with the
workload's identity and overwrites the `ruum.*` attributes and the storage tenant (review R5).
The log shipper redacts the app's secret values and the workload token by exact match before
anything else; a deny-list of field names is always `secret`; a stricter suggestion applies at
once (review R6). The raw copy is off by default and is decrypted in step 3 only into an issue by
the reporter's choice. Only a person raises `telemetry_mode` to `full` or `raw_copy_days` (review
R9). Erasure: account deletion and member removal shred the user's end-user keys in every app of
each tenant, per tenant (review R13). Read filters are escaped by the server.

### 14. Public projects wait

`getProjectPublication` and `updateProjectPublication` are in the contract (planned); card V-1
builds them after restricted folders and C-4. Published issues never carry report attachments.

### 15. The report consent runs where app code cannot reach it (review R1)

The report page is on the platform origin (`/report`, a top-level window), with a report session
from a handoff through the gateway and a CSRF token that the app origin never sees; its API is
`/.ruum-report/v1/` (gateway contract). The gateway issues visit IDs and links records only by its
own visit-to-trace map; it ignores an incoming `traceparent`. The reporter's choices are accepted
only there. Report attachments never leave the issue (copy, template, publication; review R12).

### 16. Host names are unambiguous and never reused (review R10, RC-5)

App slugs 2 to 53 characters, tenant slugs 2 to 40, no `--`, no `-` at an end, no `xn--` prefix;
`short_id` is 8 characters of lower-case base32; the reserved list of `docs/STEP3.md` applies to
tenant and app slugs; a used slug is retired (`retired_app_slugs`); a custom domain holds its host
across tenants only when verified, and a pending row expires after 7 days.

### 17. Automatic rollback counts only server-side signals (review R5)

The gateway's `5xx`, crashes and health; never browser events. New fingerprints from browser
events are capped per app and hour. Platform actions have the app as actor (review R14).

### 18. Costs follow design 21.2 and never track visitors (review R15)

Previews, builds, checks and migrations are charged to the deployer; live traffic, instances,
egress, storage and telemetry to the app and its space; visitors never appear in meters. Each card
emits its meters through `UsageSink` (W-1); I-8 stores and aggregates. At a hard cap previews stop
first, then the app is `suspended` (`over_limit`). Plan limits are configuration.

### 19. Contract details

- `PrincipalKind`, `EventType`, the deployment source kinds and stage names are open enums
  (`contracts/README.md`); `build` and `test` stages are reserved (always `skipped` in step 3).
- `schema.zed` has `permission open_preview = edit`; the gateway checks it, so step 5 widens one
  line (review RC-7). A removed app role gives neither `use` nor `grant` (S4, an assertion).
- `wait` (long poll, at most 30 s) on `getAppDeployment`, `getAppApproval`, `getTemplateSetup`.
- `.gitleaks.toml` has a rule for `ruum_appkey_` keys; partner keys and receipt tokens carry at
  least 128 random bits (review R14).
- `issue_reports.reporter_email` is deleted 30 days after the issue closes (review R13).

### 20. The cards and the order follow the reviews

The large cards are split (R-2a/b, G-1a/b, W-3a/b/c, O-1a/b, M-2a/b); new cards C-4 (admin
contract), T-6 (gateway and runtime API tests before code), R-5 (job scheduler), R-6 (event
delivery); the step 2 critical path is a table of `docs/STEP3.md` with two proposals to the step 2
planner: **split D-3** into D-3a (`schema_only` branches, no Q-2) and D-3b (masked), and a new
**D-5** (app role kinds of a binding). Milestones M1 static, M2 node, M3 data, M4 reports, each
with a benchmark task. The freeze of C-3 allows the additive contract needs of U-3 until U-3 is
accepted (review RC-14). Every step 3 service is behind the Compose profile `apps`.

## B. Answers to the open questions (numbers of `panel/step3-open.md`)

| # | Question | Answer (provisional) |
|---|---|---|
| 1 | Gateway: own binary or module | Own binary `ruum-gateway` without tenant keys, only narrow lookups; the internal listeners in one second binary `ruum-runtime-api` (decision 1). |
| 2 | App principal per environment | Yes: live = app node ID, preview = deployment ID, plus the preview token audience (decision 5). |
| 3 | Builder for `node22` | Railpack behind a `Builder` trait, builds in the sandbox (S-2 point 5; fallback: a fixed recipe without a daemon). The new binary is the person's approval (brief E-5). |
| 4 | Where the telemetry classes apply | A Rust processor, as a role of `ruum-server`; it authenticates sources and overwrites resource attributes; secret redaction happens before it. |
| 5 | Egress proxy | A small Rust CONNECT proxy behind an `EgressPolicy` trait (reviewers disagree; section C). |
| 6 | Static apps before the sandbox | Yes, as milestone M1, with G-1a's header, cookie and token rules (not before them). |
| 7 | Who files an issue | `comment` on the folder; commenters set no assignee, status or links; rate limited. |
| 8 | Agent publishes | Always wait for a person in step 3, with long poll and one decision (decision 4). |
| 9 | Agents and secret values | Never in step 3; revisit in step 5 (decision 7). |
| 10 | Issues as typed nodes | Typed nodes, with the design text change (decision 6). |
| 11 | Raw-copy grants outside reports | Not in step 3. |
| 12 | Free-text scrubbing | Patterns only, plus exact-match redaction of secret values. |
| 13 | Rollback across migrations | Forward-only with `force`; the approval page and the CLI say which schema stays. |
| 14 | Default and inactive roles | The draft, with S4 (one write, an assertion) and a paginated, cost-tested default-role listing. |
| 15 | Reserved slugs | Reserved: the draft's list plus `report`, `preview`, `app`, `apps`, `auth`, `login`, `id`, `cdn`, `assets`, `docs`, `help`, `support`, `blog`, `dev`, `staging`, `test`, `mta-sts`, `autoconfig`, every slug under 3 characters, `_` prefixes, `xn--` prefixes and every slug with `--`; for tenant and app slugs. The PSL entry is the person's (brief E-1). |
| 16 | Plan limits | The person's (brief E-2); the placeholders stay, as configuration. |
| 17 | Admin console API | A separate contract `contracts/admin/`, card C-4 before any public app, custom domain or V-1; who the admins are is the person's (brief E-3). |
| 18 | SDKs on npm | Prepared in `sdk/node/` and vendored in gallery archives (and served at a stable URL) until a person publishes (brief E-4). |
| 19 | Report widget default | On for `platform` apps, off for `public` apps until the person (a tenant manager) turns it on; the consent step on the report origin (decision 15). |
| 20 | Hint for an agent's `restoreVersion` | The hint, with the deployment ID in `error.details.deployment_id`. |
| 21 | `AppManifest` untyped in `openapi.yaml` | Accepted; the schema gets a stable URL, gallery `app.yaml` files point to it, CI validates the examples. |
| 22 | `.` becomes `\|` in scope IDs | Accepted; the scope and API name patterns exclude `\|`, so the mapping is reversible. |
| 23 | Upload ID for archives | No; `source.kind: folder` is the missing path for agents (decision 2). |
| 24 | Actor of `issue.reveal_decided` | A new `PrincipalKind` value `issue_receipt` (the bearer of a receipt token, id = the issue node); `PrincipalKind` is an open enum (reviewers disagree; section C). |

## C. Where the reviewers disagreed, and the safer option

| Topic | Reviewer 1 | Reviewer 2 | Taken |
|---|---|---|---|
| Egress proxy (Q5) | Envoy, with resolved-IP checks and no paths in logs | A Rust CONNECT proxy | **The Rust CONNECT proxy.** It connects to the IP it checked (DNS rebinding is closed by construction), its per-app allow-lists need no xDS control plane or hot restarts, it has an in-memory conformance suite like every module border, and it adds no new dependency. Reviewer 1's conditions (resolved-IP checks, no paths or query strings in logs, app identity from the network) are all in it. Envoy stays possible behind `EgressPolicy`. |
| Actor of `issue.reveal_decided` (Q24) | A principal kind `issue_receipt` | The app as actor | **`issue_receipt`.** Naming the app would record the reporter's privacy decision as the app's own act, a false audit record. Both reviewers agree the new value is only an `oasdiff` warning (open enum). |
| Binaries (Q1) | Own gateway binary without tenant keys | One binary for the gateway, the internal gateway and the runtime API | **Two binaries** (decision 1): the internet-facing gateway stays without tenant keys or content access; the internal listeners share one binary. |
| App writes to files (RC-6) | — | Add a write subset to the runtime API, or defer and cap at `view` | **Defer, cap at `view`**: no write path for untrusted code in step 3; `listBoundChildren` is added so a `view` folder binding is useful. |
| Agents and live apps (R14, S1) | Remove live use by agents | Needs a gateway credential | **Agents reach previews only**, with an agent token of audience `ruumapps`; live use waits for a consent scope. |
| Slug length (R10, RC-5) | At most 53 characters | `63 - 2 - short-id length` | Both: `short_id` is 8 characters, so 53. |

## D. Not taken (should-change items left for later, with the reason)

- `issue_source` as `text` with `CHECK` (reviewer 2, should-change 11): 0011 uses Postgres enums
  for every state type; one exception would be inconsistent. A later value uses `ALTER TYPE ...
  ADD VALUE`.
- Consistent approval envelope names (`app_id` vs `node_id`) and `listMyPendingApprovals`: a
  later additive change when step 5 adds approvals for `delete` and `share`.
- A platform redirect `/apps/{id}/drafts/{draft}` to the newest preview of a draft: backlog; the
  supersession by `draft` is in.
- `ruum app dev` with a local JWKS: part of O-5's dev mode; the CLI command is backlog.

## E. Decision briefs for the person

These are product, money or outward-facing decisions (design 18.9). Nothing waits on them before
milestone M2, except where named.

### E-1. The Public Suffix List entry for `ruumapps.net` (question 15)

- **What**: ask the PSL maintainers to add `*.ruumapps.net` (every tenant label is a public
  suffix), so `<app>.<tenant>` and `<id>--<app>.<tenant>` are separate sites and cookies cannot
  cross apps.
- **Why it is yours**: an outward-facing request in Ruum's name, together with the domain
  registration of decision 1.
- **Recommendation**: register `ruumapps.net` and file the PSL request before the first public
  app; inclusion takes weeks. Until then apps are `access: platform` only, which decision 22
  already requires.
- **Needed by**: the first public app (after C-4).

### E-2. Plans and limits of apps (question 16)

- **What**: the free tier's app limits (`AppPlanLimits`).
- **Recommendation of both reviewers**: 3 apps, 256 MB, 1 instance, scale to zero, 7 days of
  telemetry, 7 days of previews, no public apps; as configuration, so the cost agent replaces the
  numbers from staging (design 21.4) without a card.
- **Needed by**: before real users; the placeholders work for the swarm.

### E-3. Who the platform admins are (question 17)

- **What**: the people who approve public apps and custom domains per tenant (decision 22) and
  block abusive apps.
- **Recommendation**: the person alone at first, through a separate Zitadel organisation with
  passkeys only, on an origin apart from `ruum.space` (card C-4).
- **Needed by**: C-4, before any public app or custom domain.

### E-4. Publishing the SDKs to npm (question 18)

- **What**: publish `@ruum/app` and `@ruum/log` to the public npm registry.
- **Recommendation**: the swarm prepares them in `sdk/node/`; gallery templates vendor them and
  the platform serves the built file at a stable URL; a person publishes when the API is stable
  (it needs an npm organisation and a name check).
- **Needed by**: apps written outside the gallery; not blocking.

### E-5. New binaries and dependencies (design 18.4, supply chain)

- **What**: Railpack (the `node22` builder, a binary with rootless BuildKit; card R-2b), a Docker
  API client such as `bollard` (R-2a), OTLP protobuf and `tonic` (the processor, O-1b), a database
  pooler image (PgBouncer or pgcat, I-4).
- **Recommendation**: approve them as provisional; each card names the exact version. The
  fallback for Railpack is a fixed recipe (base image, `npm ci` in a sandbox job), if S-2 point 5
  shows that rootless BuildKit does not hold in the sandbox.
- **Needed by**: M2 (R-2a, R-2b, O-1b).

## F. The person's answers to the briefs (2026-11-11)

- **E-2**: the free-tier app limits are accepted as placeholders (configuration); the cost agent replaces them from
  staging costs (design 21.4).
- **E-3**: the person alone is platform admin at first, in a separate Zitadel organisation with passkeys only, on an
  admin origin apart from `ruum.space` (card C-4).
- **E-5**: Railpack (with rootless BuildKit; fixed-recipe fallback), `bollard`, OTLP protobuf with `tonic`, and a
  database pooler image (PgBouncer or pgcat) are approved; each card names the exact version.
- **E-1** (Public Suffix List request) and **E-4** (npm publishing) stay open until the first public app and a stable
  API.

## G. Spike S-2 result (2026-11-12)

Card S-2 of `docs/STEP3.md`; scripts in `spikes/step3-sandbox/` (`run.sh` re-runs every point). Build
server: Docker 29.1.3 with `runc`, Ubuntu 24.04, kernel 6.8. Plain Docker results stand on their own;
runsc is not run. Answers 3 and 5 of section B stay as they are; the points below add what the cards
must do.

| Point | Result | What the design and the cards take from it |
|---|---|---|
| 2. Gateway headers and token | **Passed** (12 checks) | G-1a strips header names that start with `ruum-` **or `ruum_`** (CGI, WSGI and PHP read `Ruum_User` as `Ruum-User`), strips hop-by-hop headers and the names listed in `Connection` before it adds its own, merges several `Cookie` headers, and matches cookie names case-insensitively after a `__Host-` or `__Secure-` prefix. It also drops every name that starts with `X-Forwarded-` (a prefix rule, like `ruum-`), `Forwarded`, `X-Real-IP`, `tracestate` and `baggage` from the internet. The gateway does **not** pass the visitor's IP address to apps (no `X-Forwarded-For`; design 9.6: apps know end users only by a pairwise pseudonym); if apps need it (rate limits), that is a contract question for C-3, not a G-1a choice. G-1a removes `Authorization` whenever the gateway authenticated the request with it (partner key or agent token, a JWT with no prefix), not by a prefix match; the spike only checked the partner key prefix. Inside the contract's rules; no contract change. |
| 4. Static archive from Garage | **Passed** (8 checks) | The key is always `<deployment prefix>/<path>`; a path segment that is empty, `.`, `..`, or has `%`, `\` or a control byte is refused, not decoded. |
| 1. Sandbox network (plain Docker, the driver's flags) | **Passed with one change** (41 checks) | **An `internal: true` network is not enough.** The host has an address on every bridge, so an app reached the host's `sshd` through its network's gateway. The Docker driver creates each app network with `-o com.docker.network.bridge.inhibit_ipv4=true` (no host address on the bridge; no root needed); then nothing outside the network was reachable. The host has no IPv4 address on such a bridge (checked on the host; Docker gives the subnet's first address to a container instead). IPv6 to the host was not reachable either: the bridge has an `fe80::` link-local address and the host a public IPv6 address, but Docker turns IPv6 off on the container's interface (`disable_ipv6` = 1 on `eth0`) of a network without IPv6. So the gateway, the runtime API, the egress proxy and the pooler are containers attached to the app network (as "Infrastructure work" 2 says), never host processes. This overrides STEP3 "Infrastructure work" 1: the option `cargo run -p ruum-gateway` beside the server is gone on Linux (a host process cannot reach app containers; it never worked on Docker Desktop either). The server's Docker driver (R-2a, a host process) cannot probe an app's readiness or health by the app's IP either: readiness goes through a container on the app network (the gateway or the runtime API) or Docker's own `HEALTHCHECK`. The shared containers on every app network (gateway, runtime API, egress proxy, pooler) listen on app-network interfaces only on their service ports, because they are the only hop between tenant networks. Internal networks resolve no external names, so the egress proxy is the only way out. The egress proxy must also deny the host's public addresses (`EGRESS_DENY`): a name such as `<host IP>.nip.io` resolves to a public address. It takes CIDR prefixes (the host's IPv6 is a `/64`) and checks the IPv4 form of an IPv4-mapped IPv6 answer (`::ffff:<host IP>`); IPv6 answers pass only inside global unicast `2000::/3` without Teredo, 6to4, benchmarking and documentation prefixes. If app networks ever get IPv6, the host path is tested again. |
| 3. Cold start (runc) | **Passed**: start of a stopped container to the first answer, median 0.25 to 0.32 s, p95 0.27 to 0.38 s (20 starts each, four runs); a new container (`docker run`) median 0.31 to 0.48 s, p95 0.32 to 0.53 s | Decision 25 (3 s) holds with a wide margin for runc. runsc numbers wait for runsc. |
| 5. Hostile build | **Passed with the fallback** (42 checks) | Rootless BuildKit does **not** start under the driver's flags (`rootlesskit: fork/exec /proc/self/exe: operation not permitted`: no user namespaces without capabilities under the default seccomp profile and the host's AppArmor rule). The build runs as **sandbox jobs** (the fixed `node22` recipe): `npm ci` in a job with the driver's flags, a tmpfs work folder, a time limit, no secrets, `HTTPS_PROXY` to the egress proxy; the hostile `postinstall` reached none of the targets; the output is a plain tar layer that runs in a sandbox. R-2b builds this recipe (append the layer to the pinned base image without a daemon). Railpack with rootless BuildKit needs a sandbox that allows user namespaces (runsc, or a runtime class on staging); revisit with runsc. |
| runsc (gVisor) | **Not run**: needs a person | Installing runsc and registering it with Docker needs root on the build server (commands in `spikes/step3-sandbox/README.md`, "runsc"). The `runsc` option of the Docker driver and its conformance run wait for that. |

Not checked by the spike: Docker's daemon-wide user namespace remapping (needs root; the spike runs
apps as user 10001 instead), Docker Desktop on macOS (the `inhibit_ipv4` behaviour may differ there;
backlog T-26 covers sandbox suite skips), and Node's own `fetch` through `HTTPS_PROXY` (the SDK and
R-2a must check that apps can use the proxy).
