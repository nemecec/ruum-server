# Step 3 (Web apps): work areas, task cards and order

Status: DRAFT, revised after the architect panel (design section 18.9), branch `s3/spec-3`.
Nothing here is built. The contracts are drafted and CI stays green with them: every new
operation is `x-ruum-status: planned`, with a standard request and isolation and permission cases
in `ruum-testkit`. The decisions are in `docs/decisions/0006-step3-spec.md` (`provisional`), with
the answers to the 24 open questions of `ruum-server-wt/panel/step3-open.md` and the decision
briefs for the person. The two panel reviews are `panel/step3-review-1.md` (security, privacy,
cost; required changes R1 to R15) and `panel/step3-review-2.md` (swarm, order, later steps,
developer experience; RC-1 to RC-15). This revision applies every required change; where the
reviews disagreed, it takes the safer option (0006 names each case).

This page splits step 3 of `docs/DESIGN.md` (section 16) into work areas and task cards for the
swarm (section 18.3). It covers design section 9 (all of it: 9.1 to 9.10), 2.7 (templates), 8.5
(agents and data, where apps touch it), 11.1 and 11.2 (where they touch apps), 17.4, 18.1 to 18.3,
19 (operations) and 21 (costs). It follows the decisions of section 17.4:

| # | Decision | What it means for this step |
|---|---|---|
| 20 | Static and Node.js first, then Python | Runtimes `static` and `node22` in the first wave; `python3.13` is a later card (R-4); `container` is not in step 3. |
| 21 | HTTP, WebSockets, scheduled jobs; uploads through the blob API | The gateway passes WebSockets; `jobs` in `app.yaml` (scheduler: card R-5); the runtime API reads and lists bound files. **App writes to files (uploads) are deferred** to a later additive change of `runtime.openapi.yaml`; in step 3 a file or folder binding is `view` (decision 0006, review RC-6). |
| 22 | Public apps and custom domains only after a Ruum admin approves, per tenant | `Tenant.app_approval` (read-only); `access: public` and `addAppDomain` answer `403` (hints `public_apps_not_allowed`, `custom_domains_not_allowed`) without it; in a team tenant only owners and admins make an app public. The admin console contract is card C-4, before any public app. |
| 23 | Internet through an egress proxy that logs and limits | A small Rust CONNECT proxy behind an `EgressPolicy` trait (decision 0006, open question 5); `egress.allow` narrows it. |
| 24 | No AI gateway in the first version | Apps call model providers through the egress proxy with their own entered secrets. |
| 25 | 1-3 s cold start is fine on cheap plans | Scale to zero (`min_instances: 0`) is the default. |
| 26 | App-level roles | `app_role` in SpiceDB; rights on objects inside an app are the app's own data. |

Step 3 also uses decisions 19 (migrations at deploy time), 30 (previews on a database branch) and
31 (tests block Publish, not the preview: step 4; the stages `build` and `test` are reserved).

## The contracts of step 3

| Contract | File |
|---|---|
| App manifest (`app.yaml`) | `contracts/manifests/app.schema.json` |
| Template, issue content, telemetry privacy file | `contracts/manifests/template.schema.json`, `issue.schema.json`, `telemetry-privacy.schema.json` |
| App gateway, runtime API, tokens | `contracts/apps/` (`README.md`, `gateway.openapi.yaml` with the report origin, `runtime.openapi.yaml`, `user-token.schema.json`, `delegated-token.schema.json`) |
| REST API | `contracts/openapi.yaml`, tags `apps`, `issues`, `templates` (45 operations, each with `x-ruum-area` and `x-ruum-status: planned`; 8 are users only) |
| MCP tools | `contracts/mcp/tools.yaml` (35 tools, not offered while planned; the 8 user-only and the 2 receipt operations are not exposed) |
| SpiceDB schema | `contracts/authz/schema.zed` (app principals, active app roles, `use`, `open_preview`, `deploy`, `publish`, `read_telemetry`, `configure`, `app_api_scope`, `anonymous:*`), `schema.test.yaml` (31 new relationships, 36 new assertions) |
| SQL | `migrations/0011_apps.sql` (draft; the integrator renumbers it if another migration lands first) |
| Journal events | `contracts/events/` (21 step 3 types in `$defs/planned_types`) |

The step 1 and step 2 operations keep their shape. `oasdiff breaking` (CI's check, `--fail-on
ERR`) reports 0 errors; the warnings are the new values of the open response enums `EventType`
and `PrincipalKind`. Additive changes to existing schemas: `Tenant.app_approval` (read-only),
`Limits.apps`, `Usage.apps`, the optional `Error.error.details` (machine-readable details such as
`deployment_id`, `binding`, `pointer`), the `PrincipalKind` value `issue_receipt`; additive text on
`copyNode`, `restoreVersion`, `createTemplate` and `updateProjectPublication`. **No new error
code**: the new refusals use existing codes with hints (`permission_denied` with
`binding_not_allowed`, `public_apps_not_allowed`, `custom_domains_not_allowed`,
`tenant_admin_required`, `publishAppDeployment`; `conflict` with `secret_missing`,
`raw_copy_gone`, `approval_stale`, `slug_retired`), so `ErrorCode` and the web copy do not
change.

**Open enums** (`contracts/README.md`, review RC-4): `PrincipalKind`, `EventType`, the source
kinds and the stage names of a deployment, and the `actor` claim of the user token are open sets.
Clients accept unknown values. The user token is open for later steps: `iss` is the instance's
origin (self-hosting), unknown claims are ignored.

### Planned operations (decision 0004-21)

The rule of step 2 holds unchanged: every step 3 operation carries `x-ruum-status: planned` until
its card builds it; `matrix::PLANNED_OPERATIONS` lists them (57 with the 12 of step 2 that are not
built yet); the card that builds an operation removes its mark and its list entry, runs
`gen-tools.mjs` (its MCP tool appears by itself), and seeds what its standard request needs in
`crates/ruum-testkit/src/fixtures.rs` (the field names stay). The same for event types
(`$defs/planned_types`).

### Operations by area

| Area (`x-ruum-area`) | Operations |
|---|---|
| `apps` | `createApp`, `getApp`, `updateAppSettings`, `listAppDeployments`, `createAppDeployment`, `getAppDeployment`, `publishAppDeployment`, `listAppSecrets`, `setAppSecret` (users only), `rotateAppSecret`, `listAppDomains`, `addAppDomain` (users only), `removeAppDomain`, `listAppApprovals`, `getAppApproval`, `decideAppApproval` (users only) |
| `roles` | `listAppRoles`, `listAppRoleGrants`, `addAppRoleGrant`, `removeAppRoleGrant` |
| `apis` | `listAppApiKeys`, `createAppApiKey` (users only), `revokeAppApiKey` |
| `telemetry` | `searchAppLogs`, `getAppMetrics`, `getAppTrace`, `compareAppDeployment`, `listAppTelemetryFields`, `confirmAppTelemetryFields` (users only), `computeAppPseudonym` (users only) |
| `issues` | `createIssue`, `listIssues`, `getIssue`, `updateIssue`, `listIssueEvents`, `createIssueRevealRequest` (users only), `resolveIssueReceipt` (public, token), `decideIssueReveal` (public, token) |
| `templates` | `listTemplates`, `getTemplate`, `createTemplate`, `instantiateTemplate`, `getTemplateSetup` |
| `publication` | `getProjectPublication`, `updateProjectPublication` (users only; waits for restricted folders) |

`getAppDeployment`, `getAppApproval` and `getTemplateSetup` take `wait` (a long poll of at most 30
seconds), so agents do not poll in a loop. The gateway paths (`/.ruum/v1/...` on app origins,
`/.ruum-report/v1/...` on the report origin) and the runtime API (`ruum.internal`) are not in
`openapi.yaml`; their acceptance tests are card **T-6**, written before the gateway code.

### Permission table for web app nodes (design 3.2, 9.2 to 9.4, 11.1)

| Action | Person | Agent (user's right ∩ scope, plus) |
|---|---|---|
| Use a live app (through the gateway) | `use` = `view` on the app node, or any active app role | **not in step 3**: a live host refuses an agent token. Live use waits for a scope flag such as `use_live_apps`, shown in consent (review S1) |
| Open a preview | `open_preview` (today `edit`) | the same, through the gateway with an agent token (audience `ruumapps`) |
| Create an app | `edit` on the folder; guests need `guests_create_resources` | `edit` (scope `max_role` edit) |
| Deploy a preview | `edit` on the app; `view` on the source file or folder; the role of every binding on its target (`binding_not_allowed`) | the same (previews never touch live data) |
| Publish, roll back | `edit`; `manage` on every database that a migration changes | **waits for a person**: `202` with an approval; the agent's user or a manager of the app decides; then the agent calls again with `approval_id` |
| Read publish requests | `edit` | `edit`, only its own requests |
| Read logs, metrics, traces, error events | `edit` | `edit`; personal fields as pseudonyms, secrets as markers, log text untrusted |
| Settings (telemetry mode, raw copy, widget, rollback) | `manage` | `manage`, but never raises `telemetry_mode` to `full` or `raw_copy_days` (`user_required`); may lower them |
| `access: public` | `manage`, owner or admin of a team tenant, platform approval | never (`user_required`) |
| Enter a secret value, turn on `preview` for it | `manage`, users only | never |
| Rotate a generated secret | `manage` | `manage` |
| Custom domain | add: `manage`, owner or admin, platform approval, users only; remove: `manage` | remove only |
| App role grants | `share` on the app, or a holder of the `grants_roles` role | as `addGrant` |
| Partner API keys | create: `manage`, users only; list, revoke: `manage` | list, revoke |
| Confirm telemetry classes | `manage`, users only (also inside `decideAppApproval`) | suggest them in `telemetry-privacy.yaml` |
| Pseudonym of a known value | `edit`, users only, rate limited | never |
| Issues | file: `comment` on the folder (a commenter sets no assignee, status or links; rate limited); read: `view`; change: `edit`; events and report attachments: `edit` | the same |
| Ask a reporter to reveal fields | `edit`, users only | never in step 3 |
| Templates | instantiate: `view` on the template (gallery: anyone signed in) and `edit` on the parent | the same |
| Public project | `manage` on folder and items, owner or admin, platform approval, users only | never |

The order of checks for an agent's publish: the user's right and the scope (`edit`), the
deployer check of every binding (live roles), the secrets that must be set, then the approval
(`202`), then (with `approval_id`) every check again, and the approval's binding to the archive
hash, the parsed manifest and the binding targets resolved at preview time (review S5).

## Shared files (not owned by any area)

As in steps 1 and 2, plus:

| Path | What |
|---|---|
| `contracts/manifests/`, `contracts/apps/` | Contract changes only. |
| `crates/ruum-core/src/apps/mod.rs` | Made by card W-1 with the module tree (`deployments`, `archive`, `bindings`, `roles`, `secrets`, `domains`, `apis`, `approvals`, `settings`, `check`) as empty files, so parallel cards edit only their file (as D-1 did, decision 0004-33). |
| `crates/ruum-core/src/usage/` (the `UsageSink` trait) | Made by W-1: a module border with an in-memory counting sink. Every card emits its own meters as it is built (review RC-10); I-8 only stores and aggregates. |
| `crates/ruum-api/src/routes/apps/mod.rs`, `routes/issues/mod.rs`, `routes/templates/mod.rs` | Made by W-1, X-1 and L-1 with `501` stubs for every handler of their tags and one submodule per card. |
| `crates/ruum-gateway/src/{lib,main}.rs` | Made by G-1a with the module tree (`host`, `session`, `check`, `token`, `static_files`, `proxy`, `telemetry`, `reports`, `domains`) as empty modules, so G-1b, G-2, G-4 and W-5 edit only their file. |
| `crates/ruum-runtime-api/src/{lib,main}.rs` | Made by R-3 with the modules `api`, `internal` (the internal gateway), `scheduler`, `events` as empty files, for G-3, R-5 and R-6. |
| `crates/ruum-core/src/runtime/mod.rs` | The `RuntimeDriver` trait (module border, design 18.1): made by R-1. Signature changes need the agreement of the `runtime` and `apps` areas. |
| `crates/ruum-core/src/runtime/builder.rs` | The `Builder` trait: made by R-1, implemented by R-2b. |
| `crates/ruum-core/src/telemetry/mod.rs` | The `TelemetryStore` and `TelemetryIngest` traits (module border): made by O-1a. |
| `crates/ruum-core/src/privacy/mod.rs` | Step 2's marker types and `Pseudonymizer`; the telemetry classes use them (O-2). Changes need the agreement of the `query` area of step 2. |

Planned marks and generated files: as in step 2 (the integrator keeps both removals and runs
`gen-tools.mjs` and `npm run gen:api` again).

### Processes

Step 3 adds **two** new binaries (decision 0006, open question 1; review RC should-change 8,
taken with the safer split of review R1):

- `ruum-gateway`: the internet-facing app gateway (app origins, previews, custom domains) and the
  report origin paths. It holds **no tenant keys and no general system access**: only the narrow
  system lookups below, SpiceDB checks, the user-token signing key and a small internal API of the
  core.
- `ruum-runtime-api`: everything at `ruum.internal` on the app networks: the runtime API, the
  internal gateway for app-to-app calls, the job scheduler (R-5) and the event delivery to
  subscribers (R-6).

The telemetry processor (question 4) is a role of the existing `ruum-server` binary (`ruum-server
telemetry`), where the tenant keys already are. Library crates as needed.

## How an app runs (the security model)

AI-written code is untrusted code (design 9.5). Eight rules, each with a test:

1. **Isolation.** Production: Kubernetes with the gVisor runtime class in a separate node pool,
   Cilium network policies, scale to zero (Knative Serving or the KEDA HTTP add-on). Laptop: the
   same `RuntimeDriver` trait with a **Docker driver** (one container per instance, no
   privileges, read-only root, a user namespace, seccomp default, no Docker socket, CPU and memory
   limits, one Docker network per app), and **runsc** (Docker with the gVisor runtime) as an
   option of that driver on Linux hosts such as the build server, with its own conformance run.
   One conformance suite runs against every driver (R-1, R-2a). On macOS the sandbox suite may
   skip where Docker Desktop differs, but a skip fails when `CI` is set (backlog T-26).
   **Builds are untrusted code too** (`npm install` runs postinstall scripts; design 10: "run
   builds in the same sandboxed pool as the apps"): the builder runs under the same driver rules
   as an app (rootless BuildKit inside the sandbox, or the build steps as sandbox jobs), with no
   secrets, no bindings, egress only through the proxy, limits on CPU, memory, time and disk, and
   a registry credential that can push once, to its own repository only (review R7).
2. **Network: default deny.** An app sandbox reaches the internal gateway (`ruum.internal`), the
   apps it binds to (through the internal gateway), the egress proxy, and its bound databases
   through **one pooler** (PgBouncer or pgcat) in front of the data cluster, never Postgres
   directly. An app role has `CONNECT` only on its own database (the `pg_hba.conf` lines for the
   app pool name the database together with the role) and **no `EXECUTE` on `ruum_mask.*`**
   (no pseudonym oracle on a masked branch; decisions 0004-24, 0006-8). Never the platform
   database, SpiceDB, OpenBao, NATS, Garage, the metadata endpoint or another tenant's sandbox.
   A hostile test (R-2a) tries each from inside a sandbox and from inside a build.
3. **The gateway checks every request** (`contracts/apps/gateway.openapi.yaml`): `use` for live,
   `open_preview` for previews. It removes `Ruum-*` headers, its own cookies (`__Host-ruum_app`,
   `__Host-ruum_visit`), partner keys and agent tokens from the forwarded request, drops an
   incoming `traceparent` (new trace, the old one as a span link), drops an app's `Set-Cookie`
   for any `ruum` name, and adds a fresh `Ruum-User` token (at most 60 s, EdDSA, a key set apart
   from the API keys). The app session is bound to the app node ID and the environment, not only
   to the host. The apps domain is another registered domain, every app origin is its own site
   (`*.ruumapps.net` on the Public Suffix List), and the gateway's cookies are `__Host-` and per
   origin.
4. **Tokens never cross.** A preview's user token has its own audience
   (`app:<app>/preview/<deployment>`) and `env: preview`; `typ` and `env` are required; the SDK
   checks `iss`, `aud`, `typ`, `env` and `exp`; `exchangeAppUserToken` refuses a token whose `aud`
   or `env` does not match the calling workload, and an `actor` other than `user` or `agent`
   without `delegate`. The internal gateway removes `Authorization` before it forwards an
   app-to-app call, so app B never sees app A's workload token (decision 0006-10; review R2, R3).
5. **No privilege escalation through bindings.** The deployer must hold every binding's role on
   its target (and the right to grant API scopes); for an agent, its user's right ∩ its scope.
   The grant records `granted_by` (decision 0006-3). An app never gets `manage` on a node; in
   step 3 a file or folder binding is `view`.
6. **Previews never touch live data** (decision 30, 0006-5): a preview is a principal of its own;
   its database bindings go to masked branches; other node bindings are capped at `view` (reads
   of live data the deployer can read); API bindings get only the called API's `preview_scopes`;
   an entered secret reaches a preview only when a user with `manage` turned on `preview` for it
   (stored state, never the manifest); previews keep the egress limits and run no jobs.
7. **Secrets are write-only** (design 9.9): `generated` (most), `entered` (a person pastes once);
   the values live only in OpenBao (KV v2, one path per app and name); the running app gets them
   as environment variables at start; no operation, log, journal entry, raw copy, build log,
   stage message or export holds a value: the log shipper redacts the current and previous secret
   values and the workload token by exact match before anything else (review R6); builds get no
   secrets. There is never a database password (bindings with short-lived roles). Honestly: an
   editor who publishes code can read entered secrets through that code; `manage` protects who
   sets a value, not who can use it, and the publish approval is the guard for agents (S7).
8. **An agent's publish waits for a person** (design 11.1, decision 0006-4): an agent can make
   previews freely and ask to publish; the person decides with the preview, the check result and
   the changes in front of them, confirms new telemetry fields and column classes in the same
   step, and that one decision also covers the D-2 migration approvals of the publish.

### Host names and slugs (review R10, RC-5)

- A preview host is `<short_id>--<app-slug>.<tenant-slug>.ruumapps.net`; `short_id` is 8
  characters of lower-case base32. App slugs are 2 to 53 characters (so the preview label fits in
  63), tenant slugs 2 to 40; neither has `--`, a `-` at either end, or an `xn--` prefix, so no app
  slug can take a preview host.
- **Reserved slugs** (tenant and app): `content`, `www`, `api`, `admin`, `static`, `internal`,
  `ruum`, `status`, `mail`, `report`, `preview`, `app`, `apps`, `auth`, `login`, `id`, `cdn`,
  `assets`, `docs`, `help`, `support`, `blog`, `dev`, `staging`, `test`, `mta-sts`,
  `autoconfig`, every slug shorter than 3 characters, and every slug starting with `_`.
- A slug used once is never given to another tenant or app (`retired_app_slugs`).
- A custom domain holds its host across tenants only when verified (`active`); a pending row is
  unique per tenant and expires after 7 days; the TXT proof decides.

### System lookups (review R11)

Four reads find the tenant before any tenant context exists. Each is a narrow function (`SECURITY
DEFINER`, returning only `(tenant_id, id)`, or a lookup table with no other data), never general
system access, and each has a mutation in T-5 (a lookup that returns other columns, or a query
without the system flag):

| Lookup | Key | Who |
|---|---|---|
| host to app and deployment | app slug, tenant slug, short ID | gateway |
| custom domain to app | host of an `active` domain | gateway |
| partner key to key | `app_api_keys_hash_idx` | gateway |
| receipt token to report | `issue_reports_receipt_idx` | core (`resolveIssueReceipt`) |

### Deploy, publish, roll back

```
agent / person           platform (apps area)                    runtime / gateway
  | createAppDeployment   |                                        |
  |---------------------->| unpack (limits) -> validate (schema,   |
  |                       | provider rules) -> bindings (resolve,  |
  |<-- 422/403 at once ---| deployer check)   [before the answer]  |
  |<-- 202 (preview) -----| -> branches for dbs -> migrations      |
  |                       | -> image (static to Garage; node22:    |
  |                       |    the builder, in the sandbox) ------>| start (driver)
  |                       | -> check (health, headless browser) <--|
  | getAppDeployment?wait | state ready, check result, stage errors|
  | publishAppDeployment  | person: go on; agent: 202 approval     |
  |---------------------->| live bindings, migrations on live      |
  |                       | (restore point, one transaction each), |
  |                       | new version (app_release_v1) --------->| start live, check, switch
  |                       |                                        | traffic; old one stops
  |                       | auto rollback on server-side errors    |
```

A version of a web app node is one **live** deployment (encoding `app_release_v1`: the archive's
chunk list and the parsed manifest; decision 0006-2), so `listVersions` is the release history and
`restoreVersion` is a rollback (people; agents use `publishAppDeployment`, and the `403` names the
deployment in `error.details.deployment_id`). Previews are rows of `app_deployments` only; their
archives stay in the blob store while the preview exists. A new preview with the same `draft`
label supersedes the older one. A rollback across a migration needs `force` and leaves the
database as it is (forward-only migrations; the approval page and the CLI say "this rollback keeps
the database schema of version N").

The **source** of a deployment is a file (an archive) or a **folder** (the platform packs the
head versions of its files, review RC-1). An agent with only MCP tools deploys with `write_file`
and then `createAppDeployment` with `source.kind: folder`. `AppDeployment.source` names the file
versions that are deployed. Step 4 adds `source.kind: build`.

The **automatic rollback** (design 9.6, layer 1 of design 19.1: no agent in the loop) compares the
server-side error rate (the gateway's `5xx` answers, the runtime's crashes and failed health
checks; **never browser events**, which anyone can send to a public app) in the 15 minutes after a
publish with the hour before; above `auto_rollback.error_rate_factor` (default 3) and above 1 %,
it publishes the previous live deployment and writes `app.rolled_back` (reason `error_rate`, actor
the app), then tells the session (step 5) and opens an issue.

**The actor of platform actions** (review R14): the automatic rollback, monitoring issues, preview
expiry, scheduled rotation and the regression check have the app as actor (`{kind: app, id: <app
node>}`), `on_behalf_of` null, the reason in `data`.

## Telemetry and privacy (design 9.6)

```
app stdout ──> runtime log shipper (exact-match secret redaction) ─┐
browser script ─> gateway /.ruum/v1/telemetry (session, visit) ────┤
OTel SDK (runtime adds it) ────────────────────────────────────────┼─> processor (ruum-server telemetry)
build and deploy events ───────────────────────────────────────────┘   authenticates the source,
                                                                       overwrites ruum.* attributes,
                                                                       classes from telemetry_fields,
                                                                       secret -> hidden marker,
                                                                       personal -> keyed pseudonym,
                                                                       raw copy (optional, per end user)
                                                                   ──> Collector ──> Loki / Mimir / Tempo
                                                                       (one storage tenant per Ruum tenant)
```

- **Sources are authenticated** (review R5): the processor accepts OTLP from a sandbox only with
  that workload's identity (the workload token or the network identity of the pod or container)
  and **overwrites** `ruum.tenant`, `ruum.app`, `ruum.deployment` and `ruum.environment`; values
  that the app sends are dropped. The storage tenant header (`X-Scope-OrgID`) comes only from
  that identity. Browser batches of an `access: platform` app need an app session.
- **Secrets never reach storage** (review R6): exact-match redaction of the app's current and
  previous secret values and the workload token in the log shipper, build logs and stage
  messages, before the classes, the pseudonym and the raw copy. A platform **deny-list** of field
  names (`authorization`, `cookie`, `set-cookie`, `password`, `token`, `secret`, `api_key` and
  similar) is always `secret`; `telemetry-privacy.yaml` cannot weaken it. An unconfirmed
  suggestion stricter than `personal` (`secret`) applies at once; a confirmation is needed only to
  relax a class.
- **Classes** follow 8.5: `secret` is never recorded (the hidden-value marker); `personal` becomes
  a keyed pseudonym (HMAC with the tenant key, the same `Pseudonymizer` as step 2, so
  `computeAppPseudonym` finds a person's records); `normal` is readable. Unclassified fields count
  as `personal`. Metric labels may only be `normal` (validation of `telemetry-privacy.yaml`, and
  ingestion drops the label). Free text without a template is `personal` and scrubbed with
  patterns only in step 3 (emails, phones, card numbers, IBANs, tokens; no name recognition).
- **The raw copy** (off by default, `raw_copy_days` up to 30; only a person raises it): each end
  user's raw `personal` values are encrypted under their own key (`telemetry_end_user_keys`,
  wrapped by the tenant key); deleting the key shreds them. In step 3 a raw value is decrypted
  only into an issue, by the reporter's own choice (`include`) or answer to a reveal request.
- **Erasure** (GDPR, review R13): when a user's account is deleted, or a tenant removes a member,
  a platform job shreds that user's pairwise end-user keys in every app of each tenant. It runs
  per tenant as the system, so no operation reads two tenants. A manager can start it for one end
  user of an app through an ops runbook in step 3 (an additive operation later). Pseudonyms stay
  until `telemetry_days` ends; the privacy notice says so.
- **Access**: `edit` on the app; every API answer marks log text as untrusted. Every filter of the
  read operations is escaped into the store's query language by the server, which builds the
  selector (tenant, app, environment); no LogQL, PromQL or TraceQL is passed through.
- **Errors reach agents without being asked** (design 9.6): level 1, the post-deploy check, is the
  stage `check` (card O-4); level 2 (push to the active session) and level 3 (the guardian) need
  agent sessions (step 5). In step 3 the issues and the journal events (`issue.created`,
  `issue.status_changed`) are the hooks that step 5 subscribes to.
- **Forged errors are capped**: browser events never count for the automatic rollback, and new
  fingerprints from browser events are capped (20 per app per hour; then they count under one
  parent issue).
- **Source maps** come in the archive (`telemetry.source_maps`), are stored privately and never
  served; the error pipeline maps frames before storage.
- **Retention** by plan (`AppPlanLimits.telemetry_days`); the storage tenant of a Ruum tenant is
  deleted with the tenant.

## Issues and reports (design 9.7)

An issue is a typed node of type `issue` (provider `ruum.issue`; decision 0006-6, with the one-line
design text change). Its content (`contracts/manifests/issue.schema.json`) is its versions;
comments are node comments; labels are tags; a board is a smart folder grouped by status. Issues
do not federate in the first version of federation (typed nodes are read-only placeholders there).

- **One concept for problems**: people, agents, monitoring and the report widget make the same
  kind of issue (`source`). The first occurrence of a fingerprint opens an issue
  (`issues_fingerprint_idx`), later ones count. A closed issue whose error returns becomes
  `regressed` (card O-3). A mute always expires (default 7 days, at most 30). Many new
  fingerprints from one publish are grouped under one parent issue (`links.deployment_id`).
- **Reports** (review R1): the consent step runs where app code cannot script it. The widget
  button on the app origin opens the **report page of the platform origin**
  (`https://ruum.space/report`) as a top-level window. The page gets a report session through a
  handoff by the gateway (a one-time code bound to the app, the deployment, the app session or
  the gateway-issued visit, and a state cookie of the page), then reads the preview and sends the
  choices to `/.ruum-report/v1/` with a CSRF token that the app origin never sees. The gateway
  issues the visit ID and keeps a visit-to-trace map; records are attached only through that map,
  never through a client's `visit_id` or `trace_id`. It applies the reporter's choices per
  `personal` field (`include` / `pseudonymise` / `remove`), holds the raw copies of the visit
  until the issue closes (at most 30 days), and makes the issue from the app's bug template.
  Non-members get a receipt token (`/r#<token>` on the platform origin, at least 128 random bits;
  `resolveIssueReceipt`, `decideIssueReveal`; the actor of a decision is `issue_receipt`). Public
  apps rate-limit reports per visitor and app. `reporter_email` is deleted 30 days after the issue
  closes.
- **Revealed values and agents** (review S2): a reporter's `include` or reveal shows the value to
  people with `edit`; agents that work on the issue see the pseudonym, unless the consent text
  says otherwise.
- **Report attachments stay private** (review R12): screenshots, attached visit records, the
  choices and revealed raw values are never copied by `copyNode`, never put in a template (also
  for issues in `include`) and never published (`updateProjectPublication`).
- **Fix in a session** (a button that opens an agent session) is step 5; step 3 keeps
  `links.session_ids` in the content for it.

## Templates (design 2.7)

- A template is a folder of type `template` with `template.yaml`. Kinds: `project` and `issue`.
- **The gallery** is bundled with the platform (`gallery:<slug>`, versioned with the release),
  never a node of another tenant, so no operation reads two tenants. It is loaded at start from a
  configured directory (default: the bundled `gallery/`) through a small `TemplateSource`, so a
  self-hoster can add templates. The first gallery: `web app + Postgres + sign-in`, `static
  site`, `internal tool` (read-only file views in step 3), and the issue templates `bug`,
  `feature`, `question`, `error` (the template of monitoring issues). Gallery templates vendor
  the SDK in the archive, start `app.yaml` with `# yaml-language-server: $schema=<stable URL of
  app.schema.json>`, and ship an `AGENTS.md` (structured logging, parameterised SQL,
  `telemetry-privacy.yaml`, the `-- ruum:classes` header).
- **Setup** (`instantiateTemplate`): folders, databases (manifest, migrations, masked sample data),
  the app from its archive with the first preview, issue templates, role placeholders. Step 4
  replaces the archive with a repo scaffold. "Update from the template" needs step 4's merge
  plugins: backlog.
- `createTemplate` follows the copy rules of typed nodes: databases as masked sample data (never
  `full`), apps without secrets; sessions, agent memory, logs and report data are never in a
  template.

## Public projects (design 9.9)

Specified now, built **after restricted folders** (design 16, decision 8):
`updateProjectPublication` writes `viewer@anonymous:*` or `viewer@user:*` on each chosen item,
never on the folder. A database item needs a public schema; `personal` columns are the hidden
marker for the public (no pseudonyms: outsiders could link records); public queries have a short
timeout, a row cap, a rate limit per visitor and caching. Issues are published without report
attachments. The extension of `queryDatabase` for anonymous callers is a later additive contract
change (card V-1). Remix stays in the backlog.

## Apps calling apps (design 9.10)

- **API bindings** (`bindings.<name>.api` + `scopes`) give the calling app's principal
  `app_api_scope:<called>/<api>/<scope>#consumer`. The deployer must have `grant` on each scope
  (`manage` on the called app, or the app role named in `granted_by`). A preview gets only the
  scopes that the called API lists in `preview_scopes`.
- **Internal** calls go to `http://<binding>.<api>.ruum.internal` through the internal gateway,
  which authenticates the caller by its workload token and network identity, removes
  `Authorization`, and adds `Ruum-User` with `actor: app`, the scopes, and for a delegated call
  `on_behalf_of` and the `chain` (at most 3 hops). Cilium allows a sandbox to reach another only
  along a binding.
- **External** APIs are published through the app gateway for users with app roles, partners
  with API keys (`createAppApiKey`, at least 128 random bits, a gitleaks rule for
  `ruum_appkey_`), or the public (with platform approval). Agents use external APIs of live apps
  only when live use by agents arrives (S1).
- **Events**: an app publishes declared topics through the runtime API; subscribers get them at
  least once at a path, with retries, a dead-letter queue and replay (NATS JetStream; card R-6).
  Ruum's own events for nodes in a binding's scope come the same way.
- **Contracts between apps**: the OpenAPI or AsyncAPI file is in the archive; the deploy warns when
  a change breaks a bound consumer (the blocking check is part of step 5's pipeline, 11.2).
- Calls between tenants only through external APIs.

## Agents and data (8.5), workspaces (11.1, 11.2)

- An app's database bindings use the app role kinds that step 2 reserved (decision 0004-32),
  built by the step 2 databases area as card **D-5** (below): the app principal gets a LOGIN role
  per binding, short-lived password, through the pooler; the privacy filter does not apply to the
  app itself (it is the owner's code with the owner's grant), but every agent that reads the same
  database through the query API still gets the lens.
- Agents do not use live apps through the gateway in step 3 (S1): a live app would show them raw
  personal data that the 8.5 lens masks in the same database. They use previews, which run on
  masked branches.
- Previews get masked branches (decision 30), so a preview never copies real personal data. Until
  D-3b lands, a preview binds a `schema_only` branch (migrations plus the manifest's seed data);
  see "Step 2 cards this step waits for".
- Migrations in `app.yaml` carry their column classes (`-- ruum:classes`); the publish approval
  lists the planned columns and unconfirmed telemetry fields in one place, and the person
  confirms both in `decideAppApproval` (`classes`, `telemetry_fields`).
- Step 3 builds what step 5's sessions need: previews, the check result, issues and their events,
  telemetry by API and MCP, and the `RuntimeDriver` with workload kinds `service` and `job` (step
  4's builds and step 5's session sandboxes add kinds, not a trait change).

## Operations (design 19)

New action types for the operations policy (`docs/ops/policy.yaml`, a person signs it; added by
card I-7 as a proposal, never in force without the person):

| Action | Class | Why |
|---|---|---|
| `app.restart` | green | within budget |
| `app.rollback` (automatic, layer 1) | green | design 9.6; also outside the swarm |
| `app.scale` within the plan | green | |
| `app.block` (abuse, phishing) | amber | design 13, 19.2 security role; two agents |
| `app.unblock` | amber | |
| `domain.revoke_certificate` | amber | |
| `tenant.approve_public_apps`, `tenant.approve_custom_domains` | red | decision 22: a person (platform admin) |
| `telemetry.delete_end_user` (crypto-shred of one end user on request) | amber | GDPR erasure, design 9.6; account deletion and member removal shred automatically |

Runbooks: a sandbox escape suspicion, an egress abuse, a phishing report against a public app, a
failing certificate renewal, an erasure request. Game days on staging (19.5) add "an app floods
the egress proxy" and "a preview tries to reach the live database".

## Costs and metering (design 21)

| Meter | Level (21.2) | Charged to | Source |
|---|---|---|---|
| instance seconds and memory of live app sandboxes | counters per app, hourly | the app and its space | runtime driver |
| instance seconds of previews | counters per app, hourly | the deployer | runtime driver |
| build minutes (`node22`) | each event | the deployer | builder |
| checks (headless browser minutes) | each event | the deployer | apps area |
| migrations at deploy | each event | the deployer | apps area |
| requests through the gateway | counters per app, hourly | the app | gateway |
| egress bytes, live and per preview | counters per app, hourly | the app; a preview's egress the deployer | egress proxy |
| image, static and archive storage | daily snapshot | the app | blob store |
| telemetry bytes ingested and stored | counters per app, daily snapshot | the app | processor, Loki/Mimir/Tempo |
| NATS JetStream storage and messages of app events | counters per app, daily | the app | runtime API |
| custom domains (certificates) | each event | the app | gateway |

Previews, builds, checks and migrations belong to the deployer (`actor` / `on_behalf_of` of the
deploy), so agent budgets and hard caps work (21.5). Live traffic, instances, egress and
telemetry are charged to the app and its space; the publish actor is metadata only. **Visitors
are never recorded in meters** (pairwise personal data; 21.5 restricts per-user views). The egress
proxy and the gateway find the app from the network or workload identity, never from a header
that the app sends. At a hard cap previews stop first; the live app keeps serving until the
space's hard cap, then it is `suspended` (reason `over_limit`). `AppUsage` has `build_seconds`,
`check_seconds` and `storage_bytes`. The plan limits of `AppPlanLimits` are configuration, not
code, and placeholders until the cost agent has staging costs (21.4); prices are a decision of the
person.

## The areas

### 1. Web apps (control plane)

| | |
|---|---|
| Owns | `crates/ruum-core/src/apps/` (`mod`, `deployments`, `archive`, `settings`, `secrets`, `domains`, `approvals`, `check`), `crates/ruum-core/src/usage/` (the trait; each area emits its own meters), `crates/ruum-api/src/routes/apps/` (except `roles.rs`, `apis.rs`, `telemetry.rs`) |
| Implements (OpenAPI) | area `apps` |
| Tables | `apps`, `app_deployments`, `app_secrets`, `app_domains`, `app_approvals`, `tenant_app_settings`, `retired_app_slugs` |
| Notes | The provider `ruum.webapp` implements `providers::Provider` (create, copy, trash = suspend, restore, purge = stop and delete images; `copied` makes a preview from the live archive). Manifest validation: the JSON Schema compiled in, plus the provider rules (plan limits, `env` vs `secrets`, one `default` role, one `grants_roles`, binding targets resolvable, `edit` only on databases, `preview_scopes` a subset of `scopes`). Secrets through a `SecretStore` trait (OpenBao KV v2; in-memory for tests). |

### 2. Bindings, roles and APIs

| | |
|---|---|
| Owns | `crates/ruum-core/src/apps/{bindings,roles,apis}.rs`, `crates/ruum-api/src/routes/apps/{roles,apis}.rs` |
| Implements (OpenAPI) | areas `roles`, `apis` |
| Tables | `app_bindings`, `app_role_grants`, `app_api_keys` |
| Contract | `schema.zed`: `app`, `app_role` (`active`), `node#active_role`, `use`, `open_preview`, `app_api_scope` |
| Notes | The deployer check; the tuples of bindings (live and preview principals); active roles follow the live manifest (`app_role#active` and `node#active_role` in one SpiceDB write, S4); database bindings call the app role kinds of step 2 card D-5 (no edits in `crates/ruum-core/src/databases/`). |

### 3. Runtime

| | |
|---|---|
| Owns | `crates/ruum-core/src/runtime/` (the `RuntimeDriver` and `Builder` traits, the in-memory driver and builder, the conformance suites), `crates/ruum-runtime/` (new crate: the Docker driver with the runsc option, the Railpack builder), `crates/ruum-runtime-api/` (new crate and binary: the runtime API, the internal gateway, the scheduler, event delivery) |
| Notes | `RuntimeDriver` models workload kinds `service` and `job` from the start (jobs, the O-4 browser check, builds). Build: static files go to Garage as they are (W-2); `node22` archives go through the builder (Railpack behind the `Builder` trait) inside the sandbox into an OCI image in a local registry. Start, stop, scale to zero (an idle timer in the driver on the laptop), logs to the processor with exact-match secret redaction, health. Workload tokens mounted and renewed. Egress only through the proxy. |

### 4. Gateway

| | |
|---|---|
| Owns | `crates/ruum-gateway/` (new crate, a binary of its own; decision 0006-1), `deploy/dev/caddy/` changes for `*.ruumapps.localhost` and the `/.ruum-report/` route of the platform origin |
| Implements | `contracts/apps/gateway.openapi.yaml`, `user-token.schema.json` |
| Notes | Host to app and deployment lookup (a narrow system lookup with a cache fed by the journal), the `use` and `open_preview` checks (SpiceDB, with a few seconds of cache per visitor and app), the app session (`__Host-ruum_app`, bound to app and environment), visits (`__Host-ruum_visit` and the visit-to-trace map), header and cookie cleaning both ways, the user token, static files from Garage, the browser script, the report origin, WebSockets, rate limits, partner API keys, custom domains with ACME (production; the laptop uses Caddy's local CA). Scale to zero wakes the app through the driver and holds the request. No tenant keys. |

### 5. Telemetry

| | |
|---|---|
| Owns | `crates/ruum-core/src/telemetry/` (traits, the in-memory store, classes, the pseudonym step, the processor logic), the `telemetry` role of `ruum-server` (`crates/ruum-server/src/telemetry.rs`), `crates/ruum-api/src/routes/apps/telemetry.rs`, `deploy/dev/otel/` (collector config) |
| Implements (OpenAPI) | area `telemetry` |
| Tables | `telemetry_fields`, `telemetry_end_user_keys` |
| Notes | The Rust processor (question 4): source authentication, attribute overwrite, the deny-list, classes from `telemetry_fields`, pseudonyms, the raw copy. The logging helpers for Node (`@ruum/log`) and the SDK (`@ruum/app`: checks `Ruum-User`), prepared in `sdk/node/` and vendored until a person publishes them. Error grouping (fingerprints), source maps, the post-deploy check (Playwright on the build host, as a `job` workload). |

### 6. Issues

| | |
|---|---|
| Owns | `crates/ruum-core/src/issues/`, `crates/ruum-api/src/routes/issues/` |
| Implements (OpenAPI) | area `issues` |
| Tables | `issues`, `issue_reports`, `issue_reveal_requests` |
| Notes | The provider `ruum.issue`; template validation (JSON Schema of the template version); mutes that expire; reports from the gateway; receipts; the `reporter_email` deletion. Counts and regressions come from the error stream (O-3). |

### 7. Templates and publication

| | |
|---|---|
| Owns | `crates/ruum-core/src/templates/`, `crates/ruum-core/src/publication/`, `crates/ruum-api/src/routes/templates/`, `crates/ruum-api/src/routes/projects.rs`, `gallery/` (new top-level folder: the bundled gallery templates) |
| Implements (OpenAPI) | areas `templates`, `publication` |
| Tables | `template_setups`, `project_publications` |

### 8. Infrastructure (laptop stack)

| | |
|---|---|
| Owns | `deploy/dev/`, `scripts/dev.sh`, `docs/DEVSTACK.md`, `scripts/buildbox/` (the runsc runtime on the build server) |
| Notes | See "Infrastructure work" below. Every step 3 service is behind the Compose profile `apps` (`dev.sh up --apps`), so step 1 and 2 work does not pay its RAM and disk. |

### 9. MCP and CLI

| | |
|---|---|
| Owns | `crates/ruum-mcp/`, `crates/ruum-cli/` |
| Implements | The 35 tools of `contracts/mcp/tools.yaml` appear by themselves as their operations land (`gen-tools.mjs`); `ruum app create|deploy|status|publish|logs|issues` with `--wait` and stage progress, `ruum template ls|new`. `ruum app deploy <dir>` packs the folder into an archive, uploads it as one file with versions (`<project>/.ruum/<app>.tar.gz`) and calls `createAppDeployment`; MCP-only agents use `source.kind: folder`. Log lines and issue text are untrusted data in every tool result. |

### 10. Web UI

| | |
|---|---|
| Owns | `web/` except `web/src/api/schema.d.ts` |
| Implements | The app page (status, address, deployments with Publish and Rollback, previews, settings, secrets with "enter value" and the preview switch, domains, roles in the share dialog, API keys), the logs and metrics views, telemetry and column classes in the publish approval, the issue list and board, the issue page, "New project from template", the receipt page (`/r#token`), the **report page** (`/report`, the consent step). UX package first (card U-3). |

### 11. Suites (test harness)

| | |
|---|---|
| Owns | `crates/ruum-testkit/` |
| Implements | Isolation and permission cases for the 45 operations (they exist, reported as planned); the fixture seeding (each card seeds what its operations need); the **gateway and runtime API harness** (T-6); a **sandbox suite** (hostile apps and hostile builds: reach the platform database, another tenant's database through the pooler, `postgres` and `template1`, `ruum_mask.pseudonym`, another tenant's app, the metadata endpoint, OpenBao, the Docker socket; send `Ruum-User` from outside; replay a user token at another app or from a preview at the live app; read another app's secrets); a **telemetry privacy suite** (no raw `secret` value is ever stored, also through `console.log(process.env)`; no raw `personal` value reaches a reader without a report choice; metric labels with personal values are dropped; forged `ruum.*` attributes are overwritten); mutations for each rule. |

## Infrastructure work (laptop stack)

1. **The app gateway** as a compose service built from `crates/ruum-gateway` (or `cargo run -p
   ruum-gateway` beside the server), with Caddy's `*.ruumapps.localhost` block proxying to it
   (today it answers `503 not built yet`) and the platform origin's `/.ruum-report/` route. The
   runtime API binary listens on the app networks as `ruum.internal`.
2. **The runtime on the laptop**: the Docker driver talks to the Docker engine through a
   **socket proxy** that allows only container, image and network calls under the label
   `ruum.app` (never the raw socket in the server). One Docker network per tenant app,
   `internal: true`, with the runtime API, the egress proxy and the database pooler attached.
   Optional: the `runsc` runtime (gVisor) where the host is Linux (the build server);
   `dev.sh env` prints `RUUM_RUNTIME_DRIVER=docker|runsc`.
3. **A local OCI registry** (`registry:2` on 127.0.0.1:5001) with a size cap, and the builder
   (Railpack with rootless BuildKit in a sandbox container), with a BuildKit cache limit and the
   pruning of images labelled `ruum.app` and of expired previews. The disk filled once from agent
   builds.
4. **The egress proxy**: a small Rust CONNECT proxy (question 5): identifies the app by its
   network, resolves the host, refuses private, link-local, metadata (169.254.169.254, fd00::/8)
   and platform ranges, **connects to the checked IP** (no DNS rebinding), checks `egress.allow`,
   counts bytes and logs host, port, bytes and status (never paths or query strings). Behind an
   `EgressPolicy` trait with an in-memory conformance suite; Envoy stays possible for production.
5. **Telemetry stack**: the processor role of `ruum-server`, the OpenTelemetry Collector plus
   Loki, Mimir (or Prometheus on the laptop) and Tempo; the single-container `grafana/otel-lgtm`
   image is enough on the laptop (with its pinned version), with multi-tenancy headers.
6. **OpenBao KV v2** for app secrets (OpenBao already runs in dev mode on the laptop; secrets of
   the laptop are lost on restart, which is fine for dev; `dev.sh seed` sets the fixture's).
7. **NATS JetStream** streams for app events (NATS already runs).
8. **A headless browser** for the post-deploy check: Playwright's Chromium (the build server has
   it for `e2e_browser.sh`).
9. **A database pooler** (PgBouncer or pgcat) in front of the data cluster on the app networks.
10. **CI**: a job that runs the sandbox suite with the Docker driver (GitHub's runners have
    Docker; runsc can be installed there too; a skip fails in CI), the T-6 harness, `redocly lint`
    for `contracts/apps/*.openapi.yaml`, `ajv` for every manifest's and token's `examples`.
11. **Staging**: gVisor runtime class, Cilium policies, Knative or KEDA scale to zero; not before
    the staging step (decision 5 needs a person for the Hetzner account).

## Fixtures

`crates/ruum-testkit/src/fixtures.rs` has the IDs now (placeholders until seeded): in each tenant
the app `<prefix>-planner` (W) in `Projects`, its live deployment, an `approver` grant to
`guest_view`, a custom domain, a partner API key, a pending publish request of `agent` (seeded
through the store: its `view` scope cannot publish through the API), a trace; the `report` issue
`<prefix>-bug` (I) with a receipt token; the project template `<prefix>-kit` (K) whose setup makes a
database and an app, and the setup that made `<prefix>-from-kit`. Both fixture tenants have the
platform admin's approval of public apps and custom domains (so the permission rules test the
positive case). The seeded W has one live deployment with the roles `member` (default) and
`approver`, the secrets `STRIPE_KEY` (entered) and `SESSION_KEY` (generated), the external API
`orders` (`orders.read`, `orders.write`; `preview_scopes: [orders.read]`) and a binding `db` with
role `edit` on D.

## Step 2 cards this step waits for (review RC-8)

Everything in step 3 that touches a database waits on step 2. On `main`, Q-2 is not merged, and
A-2, D-2 and D-3 have not started.

| Step 3 needs | Step 2 card | Waits for | Used by |
|---|---|---|---|
| `schema_only` branches for previews | **D-3a** (proposed split of D-3: `schema_only` branches; after D-1, A-1, C-2; no masking, so no Q-2) | D-1, A-1, C-2 | W-2 (database bindings), L-2, M3 |
| `masked` branches for previews | **D-3b** (the masked part of D-3, and the `database.branch_created` journal entry that D-3a leaves out) | Q-2 | previews with real-shaped data |
| migrations at publish | D-2 | A-2 | W-3a |
| the app role kinds of a binding | **D-5** (proposed: "app role kinds of a binding", `RoleRef` with a binding key, decision 0004-32, in `crates/ruum-core/src/databases/`) | D-1 | B-1 |

The split of D-3 and the new D-5 are proposals to the step 2 planner (step 2 card changes, not
part of 0011). Until D-3a lands, a preview with a database binding fails at stage `bindings` with
`conflict`, hint `database_branches_not_ready`; until D-3b lands, previews bind `schema_only`
branches (migrations plus the manifest's seed data), which is already "a branch" under decision
30.

## Milestones (review RC-8)

| Milestone | What works | Cards | Benchmark task |
|---|---|---|---|
| **M1 static** | a static app live, deployed over MCP with `source.kind: folder` | W-1, W-2 (static), W-3a (publish without migrations), G-1a, B-2, M-2a | "deploy a static site over MCP" |
| **M2 node** | a `node22` app without a database, live, with logs | R-1, R-2a, R-2b, G-1b, R-3, O-1a, O-1b, O-2 | "deploy a Node app and read its error log" |
| **M3 data** | the template "web app + Postgres + sign-in" with a preview and a publish | D-3a, D-2, D-5, B-1, W-3b, L-1, L-2 | "start from the template, add a column, publish" |
| **M4 reports** | the report widget, issues, receipts | O-3, X-1, X-2, G-2, G-4, U-4 (report page) | "file a report and fix the issue" |

## Task cards

IDs: `C` contract, `S` spike, `I` infrastructure, `W` web apps (control plane), `B` bindings, roles
and APIs, `R` runtime, `G` gateway, `O` observability (telemetry), `X` issues, `L` templates, `V`
public projects, `M` MCP and CLI, `U` UI, `T` tests. Numbers continue the step 2 numbers where
the letter is shared (C, S, I, M, U, T, D). "After" lists the cards that must be merged first.
Every card that builds an operation also removes its planned mark and its `PLANNED_OPERATIONS`
entry, regenerates `tools.json`, and seeds its fixture fields (not listed again under "May
change"). Every card emits its own meters through `UsageSink`.

```markdown
ID:          C-3
Goal:        Review and freeze the step 3 contracts (this draft, revised after the panel).
Contracts:   contracts/manifests/{app,template,issue,telemetry-privacy}.schema.json,
             contracts/apps/*, the tags apps, issues, templates of openapi.yaml, schema.zed,
             mcp/tools.yaml, migrations/0011_apps.sql, the step 3 event schemas.
May change:  those files and the generated files; crates/ruum-testkit/ (the lists, the fixture
             fields, standard requests, isolation cases, permission rules, permission_matrix.md);
             crates/ruum-mcp/tests/content_trust.rs (UNTRUSTED_FIELDS); .gitleaks.toml;
             docs/STEP3.md; docs/decisions/0006-step3-spec.md.
Must not:    product code.
Done when:   the architect panel accepts (two reviewers, one with the security lens); redocly
             (openapi.yaml and contracts/apps/*.openapi.yaml), zed validate, oasdiff (0 errors),
             the migration applies on a scratch database, ajv for the manifest, token and event
             examples; cargo test --workspace and RUUM_REQUIRE_IMPLEMENTED=1 cargo test -p
             ruum-testkit (memory and postgres) pass; the open questions of panel/step3-open.md
             have a provisional answer in 0006. The freeze allows the additive contract needs of
             U-3 until U-3 is accepted (a named exception to "frozen", review RC-14).
After:       none
Out of scope: building anything.
```

```markdown
ID:          C-4
Goal:        The platform admin console contract (contracts/admin/): approve public apps and
             custom domains per tenant (decision 22), block and unblock apps.
Contracts:   contracts/admin/ (new), tenant_app_settings.*_approved_by.
May change:  contracts/admin/, docs/STEP3.md, 0006.
Done when:   the panel accepts; admins sign in through their own Zitadel organisation with
             passkeys only, on an origin apart from ruum.space; who the admins are is the
             person's decision (0006 brief).
After:       C-3. Before any public app, custom domain or V-1.
```

```markdown
ID:          S-2
Goal:        Spike: the sandbox, the builder and the gateway on the build server, before R-1 and
             G-1b freeze.
Contracts:   docs/STEP3.md "How an app runs"; contracts/apps/.
May change:  spikes/step3-sandbox/ (scripts and a short README), docs/decisions/0006 (result).
Must not:    crates/, contracts (a contract change the result needs goes to C-3).
Done when:   on the build server, a script shows: (1) a Node app in Docker with runsc (gVisor)
             and in plain Docker with the driver's flags cannot reach the platform Postgres,
             SpiceDB, OpenBao, the metadata address, the Docker socket or another app's network,
             and reaches the internet only through the egress proxy; (2) a small Rust proxy in
             front of it strips an incoming Ruum-User header and the gateway cookies and adds a
             signed one; (3) a cold start from a stopped container answers within 3 s for a small
             Node app (decision 25), with numbers for runsc and plain Docker; (4) a static archive
             served from Garage through the proxy; (5) a hostile package.json (a postinstall that
             calls the metadata address, the platform Postgres and the Docker socket) builds in a
             sandbox (rootless BuildKit with the driver's flags, or the build steps as sandbox
             jobs) and reaches nothing. Points 2 and 4 report first (they unblock G-1a). If a
             point fails, the result names the change; for (5) the fallback is a fixed node22
             recipe (base image, npm ci in a sandbox job, layers appended without a daemon).
After:       none
Out of scope: Kubernetes, Cilium, Knative (staging).
```

```markdown
ID:          T-4
Goal:        Acceptance tests of the 45 step 3 operations, before the code (the standard
             requests, isolation cases and permission rules already exist from C-3, reported as
             planned), and the sandbox and telemetry privacy suites as planned cases.
Contracts:   openapi.yaml tags apps, issues, templates; schema.zed; contracts/apps/.
May change:  crates/ruum-testkit/ (extra cases, the sandbox and telemetry suites),
             crates/ruum-api/tests/acceptance/apps_*.rs, issues_*.rs, templates_*.rs (new).
Must not:    product code, contracts.
Done when:   every operation has at least one extra case beyond the matrix (deployer check,
             the direct 422 and 403 of createAppDeployment, preview on a branch, preview scopes,
             agent publish waits, an approval bound to its archive, secrets never in any answer,
             app role grants and the active flag, telemetry classes, stricter suggestions apply at
             once, an agent cannot raise telemetry_mode or raw_copy_days, slug rules and retired
             slugs, a pending custom domain does not block another tenant, mutes that expire,
             reveal requests, gallery templates); an isolation case: a binding to node:<uuid> of
             another tenant fails like a missing node; an injection case for searchAppLogs q; all
             report planned on main; a person or the panel reviews them (design 18.2).
After:       C-3
Out of scope: the browser check (O-4 brings its own tests); the gateway paths (T-6).
```

```markdown
ID:          T-6
Goal:        Tests before code for the gateway and the runtime API (design 18.2): a black-box
             harness that runs ruum-gateway or ruum-runtime-api against a fake echo app and the
             in-memory core, with planned cases.
Contracts:   contracts/apps/*.
May change:  crates/ruum-testkit/src/gateway/ (new), crates/ruum-testkit/tests/gateway*.rs.
Must not:    product code, contracts.
Done when:   planned cases exist for: header, cookie and Authorization stripping both ways;
             Set-Cookie of ruum names dropped; incoming traceparent replaced; use and
             open_preview; a token per app (aud) and per environment (env); a preview's token
             refused by the live app and by exchangeAppUserToken; replay across the three
             credentials (0006-10); a session of app A refused by app B on a reused host; a
             removed role failing within 5 s; an agent token refused on a live host; the report
             origin (an app script calls the preview, posts include choices, names another visit:
             all refused); receipts; the reserved paths; the internal gateway removes the
             caller's workload token. I-6 runs them in CI.
After:       C-3 (parallel to T-4)
```

```markdown
ID:          I-4
Goal:        The runtime on the laptop: Docker socket proxy, app networks, local registry,
             builder image, egress proxy, database pooler; runsc on the build server; the Compose
             profile apps.
Contracts:   docs/STEP3.md "Infrastructure work" 2 to 4 and 9.
May change:  deploy/dev/, scripts/dev.sh, docs/DEVSTACK.md, scripts/buildbox/ (setup only).
Must not:    crates/ (the egress proxy crate is R-2a's).
Done when:   `dev.sh up --apps` starts the socket proxy, the registry, the egress proxy and the
             pooler; `dev.sh up` without it starts none of them; `dev.sh env` prints
             RUUM_RUNTIME_DRIVER, RUUM_DOCKER_PROXY, RUUM_REGISTRY, RUUM_EGRESS_PROXY,
             RUUM_DB_POOLER; a check script (`scripts/sandbox_check.sh`) runs a hostile container
             on an app network and shows that every forbidden address of R-2a is unreachable; the
             registry has a size cap, the BuildKit cache a limit, and a prune job removes images
             labelled ruum.app of expired previews; the build server has runsc registered (a
             person runs the setup if it needs root, decision 18.9).
After:       S-2
Out of scope: Kubernetes.
```

```markdown
ID:          I-5
Goal:        The telemetry stack on the laptop: OTel Collector and Loki/Mimir/Tempo (or the
             single grafana/otel-lgtm container), with per-tenant headers, in the profile apps.
May change:  deploy/dev/, deploy/dev/otel/ (collector config skeleton), scripts/dev.sh,
             docs/DEVSTACK.md.
Done when:   `dev.sh up --apps` starts it; a test record with a tenant header is readable only
             with that header; `dev.sh env` prints RUUM_OTLP_ENDPOINT and the query URLs.
After:       none
```

```markdown
ID:          I-6
Goal:        The app gateway and the runtime API in the laptop stack (Caddy wiring for
             *.ruumapps.localhost and /.ruum-report/, the ruum.internal name on app networks),
             and CI jobs for the sandbox suite, the T-6 harness and the contract lints of
             contracts/apps.
May change:  deploy/dev/caddy/, deploy/dev/docker-compose.yml, .github/workflows/ci.yml (new
             jobs only), docs/DEVSTACK.md.
Done when:   https://<app>.<tenant>.ruumapps.localhost reaches the gateway; the CI jobs run
             (planned cases pass; a skipped sandbox test fails in CI).
After:       I-4, G-1a
```

```markdown
ID:          I-7
Goal:        Operations: the app action types and runbooks of docs/STEP3.md "Operations" as a
             proposal to the signed policy (never in force without the person), including the
             erasure runbook.
May change:  docs/ops/ (proposal file and runbooks only).
Done when:   the guard review of the ops area accepts the proposal; the person's approval is
             listed as pending in the digest.
After:       W-3a
```

```markdown
ID:          I-8
Goal:        Metering of apps (design 21): store and aggregate the meters of docs/STEP3.md
             "Costs and metering" that the cards emit through UsageSink; fill Usage.apps.
May change:  crates/ruum-core/src/usage/ (the Postgres sink and the aggregation).
Done when:   usage events carry tenant, app, the charged principal (deployer or app) and never a
             visitor; Usage.apps is filled; hard caps stop previews first, then suspend the app
             (over_limit); the cost of measuring stays a counter per app per hour.
After:       W-1 (the trait); then any time
```

```markdown
ID:          W-1
Goal:        The web app provider and the control plane skeleton: createApp, getApp,
             updateAppSettings, listAppDeployments, getAppDeployment (with wait); the module tree
             with 501 stubs for every handler of the tag apps; the UsageSink trait.
Contracts:   openapi.yaml area apps; contracts/manifests/app.schema.json; 0011 (apps,
             app_deployments, tenant_app_settings, retired_app_slugs).
May change:  crates/ruum-core/src/apps/{mod,settings}.rs, the empty files of the other
             submodules, crates/ruum-core/src/usage/ (trait, in-memory sink),
             crates/ruum-api/src/routes/apps/mod.rs, crates/ruum-core/src/services.rs (one line),
             crates/ruum-core/src/providers/ (registering ruum.webapp only).
Done when:   T-4's cases of these operations pass in memory and Postgres; access public refused
             without the tenant approval and for members of a team tenant; an agent cannot raise
             telemetry_mode or raw_copy_days; slug rules, reserved and retired slugs; copy and
             trash hooks of ruum.webapp (copy = new app with a preview, trash = suspended).
After:       C-3
```

```markdown
ID:          W-2
Goal:        Deployments: createAppDeployment (archive from a file node or a folder, unpack,
             validate and bindings before the 202, the stages with machine-readable errors,
             previews with expiry and draft supersession), the image stage for static (archive to
             Garage, content-addressed), through the RuntimeDriver trait.
May change:  crates/ruum-core/src/apps/{deployments,archive}.rs, routes/apps/deployments.rs.
Done when:   T-4's deployment cases pass with the in-memory driver; a bad manifest is a direct 422
             with a pointer, a refused binding a direct 403; zip and tar slip, links, device files
             and archive bombs are refused (hostile tests); a folder source records its file
             versions; a static preview works end to end with the in-memory driver; a preview
             binds a branch (with D-3a) and caps other bindings at view.
After:       W-1, R-1, B-1 (database bindings: D-3a; static previews need neither)
```

```markdown
ID:          W-3a
Goal:        Publish and roll back: publishAppDeployment for people, restoreVersion of an app
             version (people; the agent's 403 names the deployment), app_release_v1 versions,
             migrations at publish (decision 19).
May change:  crates/ruum-core/src/apps/deployments.rs (publish part), routes/apps/deployments.rs,
             crates/ruum-api/src/routes/versions.rs (the app branch of restoreVersion only).
Done when:   a failed migration leaves the live deployment and the database as they were; a
             rollback across a migration needs force; static publishes work before D-2 (no
             migrations).
After:       W-2 (migrations: D-2)
```

```markdown
ID:          W-3b
Goal:        The approvals of agents' publishes: listAppApprovals, getAppApproval (with wait),
             decideAppApproval (telemetry fields and column classes), the binding of an approval
             to the archive, the manifest and the resolved bindings; one decision covers the D-2
             migration approvals.
May change:  crates/ruum-core/src/apps/approvals.rs, routes/apps/approvals.rs.
Done when:   an agent's publish answers 202 and runs only with an approved approval_id, once,
             within 15 minutes, by the agent that asked; a publish that resolves anything else
             fails (approval_stale); the decider needs manage on every changed database;
             mutations for "an agent publishes without approval", "an approval is used twice"
             and "an approval is used for another archive".
After:       W-3a
```

```markdown
ID:          W-3c
Goal:        The automatic rollback on server-side signals (the gateway's 5xx, crashes, health).
May change:  crates/ruum-core/src/apps/rollback.rs (new).
Done when:   it fires on a scripted burst of 5xx; it never fires on browser events (a forged
             error burst through /.ruum/v1/telemetry changes nothing); it writes app.rolled_back
             with the app as actor and opens an issue.
After:       W-3a, O-2, X-1, G-1b
```

```markdown
ID:          W-4
Goal:        Secrets: listAppSecrets, setAppSecret (value and the preview flag), rotateAppSecret;
             the SecretStore trait (OpenBao KV v2 and in-memory); generated secrets on deploy;
             scheduled rotation; the secret list that the log shipper redacts.
May change:  crates/ruum-core/src/apps/secrets.rs, routes/apps/secrets.rs.
Done when:   no answer, log line, journal entry, audit entry, export, log store record, raw copy,
             build log or stage message holds a value (a test greps every output of the suites
             for the fixture values, including console.log(process.env) in a fixture app);
             previews get an entered secret only when a user turned on its preview flag; builds
             get no secrets; the running app gets the values at start.
After:       W-1, R-1
```

```markdown
ID:          W-5
Goal:        Custom domains: listAppDomains, addAppDomain, removeAppDomain; DNS (TXT) checks;
             ACME in the gateway (production), Caddy's local CA on the laptop.
May change:  crates/ruum-core/src/apps/domains.rs, routes/apps/domains.rs,
             crates/ruum-gateway/src/domains.rs (made empty by G-1a).
Done when:   a host under a Ruum domain or an active host of another tenant is refused (409, no
             tenant named); a pending host of another tenant does not block (the TXT proof
             decides; pending rows expire after 7 days); without the approval 403
             custom_domains_not_allowed; the gateway serves an active domain.
After:       W-1, G-1a, C-4
```

```markdown
ID:          B-1
Goal:        Bindings: resolution of paths and IDs at deploy time, the deployer check, the tuples
             of live and preview principals, app_bindings, preview scopes, the end of bindings.
May change:  crates/ruum-core/src/apps/bindings.rs (database bindings call D-5; no edits in
             crates/ruum-core/src/databases/).
Done when:   a deployer without the role is refused (binding_not_allowed); an agent's binding is
             checked against its user's right ∩ scope; a file binding with edit is refused; a
             preview gets only preview_scopes; a dropped binding removes its tuple and writes
             app.binding_revoked; a preview never gets a live database; sandbox cases: another
             tenant's database with the app's own credentials, postgres and template1, and
             ruum_mask.pseudonym all fail; mutations for each rule.
After:       C-3, W-1 (database bindings: D-5)
```

```markdown
ID:          B-2
Goal:        App roles: listAppRoles, listAppRoleGrants, addAppRoleGrant, removeAppRoleGrant;
             the active flag follows the live manifest; the gateway's roles claim.
May change:  crates/ruum-core/src/apps/roles.rs, routes/apps/roles.rs.
Done when:   a role removed by a new version keeps its grants and gives nothing (neither use nor
             grant); a rollback makes it active again; the two tuples are one SpiceDB write; a
             grants_roles holder grants; the default role is given to viewers without an
             explicit role; listAppSelfUsers?role=<default> is paginated and has a cost test.
After:       W-1
```

```markdown
ID:          B-3
Goal:        APIs between apps: API bindings and scopes (app_api_scope), preview scopes, the
             internal gateway headers and the delegation chain (with G-3), partner API keys
             (listAppApiKeys, createAppApiKey, revokeAppApiKey).
May change:  crates/ruum-core/src/apps/apis.rs, routes/apps/apis.rs.
Done when:   a call without a binding is refused at the network and at the gateway; a chain
             longer than 3 hops is refused; a partner key works only for its API, scopes and
             rate; a revoked key stops within seconds; keys have at least 128 random bits.
After:       B-1, G-3
```

```markdown
ID:          R-1
Goal:        The RuntimeDriver trait (workload kinds service and job), the Builder trait, the
             in-memory driver and builder, and the conformance suites.
May change:  crates/ruum-core/src/runtime/.
Done when:   the suite covers build, start, stop, scale to zero and wake, run-to-completion jobs,
             logs, health, limits and the network rules as observable by the driver; the
             in-memory driver passes; a skipped case fails when CI is set.
After:       C-3
```

```markdown
ID:          R-2a
Goal:        The Docker driver (crates/ruum-runtime) with runsc as a runtime option, the egress
             proxy (a Rust CONNECT proxy behind EgressPolicy), and the hostile sandbox suite.
May change:  crates/ruum-runtime/ (new crate), crates/ruum-egress/ (new crate); new dependencies
             need the person's approval (a Docker API client such as bollard).
Done when:   the driver passes the conformance suite on the build server with plain Docker and
             with runsc (two runs); the sandbox suite passes (every forbidden address
             unreachable; no privilege; read-only root); the proxy refuses private and metadata
             ranges after resolution, connects to the checked IP and logs no paths; mutations
             that remove each driver flag fail a test.
After:       R-1, I-4, S-2
```

```markdown
ID:          R-2b
Goal:        The Railpack builder behind the Builder trait; builds run in the sandbox (rule 1).
May change:  crates/ruum-runtime/src/builder/ (new dependency: Railpack, a binary; the person
             approves it, 0006 brief).
Done when:   a node22 archive becomes an image in the local registry; a hostile build (a
             postinstall that tries the forbidden addresses of R-2a) reaches nothing; a build has
             no secrets and no bindings; the registry credential pushes once to its own
             repository; build minutes are emitted.
After:       R-2a
```

```markdown
ID:          R-3
Goal:        The runtime API binary (crates/ruum-runtime-api): getAppSelf, listAppSelfUsers,
             exchangeAppUserToken (the delegated token), readBoundContent, listBoundChildren,
             publishAppEvent; workload tokens; the module tree for G-3, R-5, R-6.
May change:  crates/ruum-runtime-api/ (new crate).
Done when:   contracts/apps/runtime.openapi.yaml is served; an app token is refused by the REST
             API and an API token by the runtime API; the exchange gives the user's right ∩ the
             binding and refuses a token whose aud or env does not match the caller, or whose
             actor is not user or agent without delegate; the delegated token follows
             delegated-token.schema.json.
After:       R-1, B-1
```

```markdown
ID:          R-4
Goal:        The python3.13 runtime (decision 20, "then Python").
May change:  crates/ruum-runtime/ (builder), the manifest provider rule that accepts it.
Done when:   a Python app passes the deployment cases; app.schema.json already lists it.
After:       R-2b
```

```markdown
ID:          R-5
Goal:        The scheduler of jobs (decision 21): calls the job path through the internal gateway
             with an app-actor user token.
May change:  crates/ruum-runtime-api/src/scheduler.rs.
Done when:   T-6 cases: one run at a time, the timeout, skipped runs while a run goes on or the
             app is suspended, previews run no jobs.
After:       R-3
```

```markdown
ID:          R-6
Goal:        Event delivery to subscribers (design 9.10): at least once, retries, dead letters,
             replay (NATS JetStream), and Ruum's own node events for a binding's scope.
May change:  crates/ruum-runtime-api/src/events.rs.
Done when:   a subscriber gets every event at least once; a failing path goes to the dead-letter
             queue after the retries; replay works; an app receives only events of its bindings.
After:       R-3, B-3
```

```markdown
ID:          G-1a
Goal:        The app gateway without the sandbox: host lookup (narrow system lookup, cache fed by
             the journal), app session and sign-in bound to app and environment, visits, the use
             and open_preview checks, header and cookie cleaning both ways, the Ruum-User token
             (preview audience), static files from Garage, previews; the module tree.
May change:  crates/ruum-gateway/ (new crate), crates/ruum-core/src/identity/ (an app audience
             for the code flow, agreed with the identity area).
Done when:   T-6's cases of these rules pass; a request without use never reaches the app; a
             token for app A is refused by app B's SDK, a preview's token by the live app; a
             removed role stops working within 5 s; an agent token is refused on a live host.
After:       C-3, T-6, S-2 points 2 and 4
```

```markdown
ID:          G-1b
Goal:        The gateway in front of sandboxes: proxy to the runtime, WebSockets, scale-to-zero
             wake and hold, status pages.
May change:  crates/ruum-gateway/src/proxy.rs.
Done when:   T-6's proxy cases pass; a cold start is held and answered; WebSockets pass the
             same check.
After:       G-1a, R-2a
```

```markdown
ID:          G-2
Goal:        The browser script and frontend telemetry (/.ruum/v1/script.js, /telemetry):
             privacy mode by default, full mode by setting; the report widget button.
May change:  crates/ruum-gateway/src/telemetry.rs, gateway/browser/ (the script source, built
             to one file).
Done when:   in privacy mode no query string, IP, cookie, body or form value leaves the browser
             or the gateway (a test inspects the stored records); a platform app refuses a batch
             without a session; body values for visit, app or tenant are ignored; events are rate
             limited.
After:       G-1a, O-1a
```

```markdown
ID:          G-3
Goal:        The internal gateway for app-to-app calls (in ruum-runtime-api) and partner keys at
             the edge (in ruum-gateway).
May change:  crates/ruum-runtime-api/src/internal.rs, crates/ruum-gateway/src/check.rs (partner
             keys only, agreed with G-1a).
Done when:   with B-3's cases; the caller's Authorization never reaches the called app.
After:       G-1a, R-3
```

```markdown
ID:          G-4
Goal:        Reports on the report origin: the handoff, createReportSession, the preview and
             createAppReport (the visit's records by the visit-to-trace map, the choices, the raw
             copy hold, the receipt token).
May change:  crates/ruum-gateway/src/reports.rs.
Done when:   T-6's report cases pass: an app script cannot read the preview, post choices or get
             a code; only the visitor's own records are offered; include works only with a raw
             copy and only into the issue; non-members get a receipt token once; public apps are
             rate limited.
After:       G-1a, O-3, X-2
```

```markdown
ID:          O-1a
Goal:        Telemetry traits, the in-memory store, the classes, the deny-list, the privacy suite.
May change:  crates/ruum-core/src/telemetry/.
Done when:   the telemetry privacy suite passes in memory for logs, metrics and traces: no raw
             secret is stored, deny-listed names are secret, a stricter suggestion applies at
             once, personal values are pseudonyms, unclassified is personal, personal metric
             labels are dropped; mutations for each.
After:       C-3, Q-4 (step 2: tenant keys)
```

```markdown
ID:          O-1b
Goal:        The processor (question 4) as the telemetry role of ruum-server: source
             authentication, attribute overwrite, the storage tenant from the identity; the
             collector config.
May change:  crates/ruum-server/src/telemetry.rs (new), crates/ruum-core/src/telemetry/
             (processor part), deploy/dev/otel/. New dependencies (OTLP protobuf, tonic): the
             person approves them (0006 brief).
Done when:   hostile cases: an app that sends another app's or tenant's ruum.* attributes, or a
             preview that claims live, writes only into its own store as itself; the storage
             tenant comes only from the identity.
After:       O-1a, I-5
```

```markdown
ID:          O-2
Goal:        Telemetry classes and reads: listAppTelemetryFields, confirmAppTelemetryFields,
             computeAppPseudonym, searchAppLogs, getAppMetrics, getAppTrace,
             compareAppDeployment; telemetry-privacy.yaml from the archive.
May change:  crates/ruum-core/src/telemetry/{classes,query}.rs, routes/apps/telemetry.rs.
Done when:   T-4's telemetry cases pass; log answers carry content_trust untrusted; filters are
             escaped (the injection case); a trace shows another app's spans only with edit on
             that app.
After:       O-1b, W-2
```

```markdown
ID:          O-3
Goal:        Errors and the raw copy: fingerprints, error groups into issues (with X-1),
             regressions, the cap on browser fingerprints, source maps, the per-end-user keys,
             crypto-shredding and the erasure job.
May change:  crates/ruum-core/src/telemetry/{errors,raw_copy,erasure}.rs.
Done when:   one fingerprint makes one issue and counts; a closed error issue reopens as
             regressed; more than 20 new browser fingerprints in an hour group under one parent;
             a deleted end-user key makes every raw value of theirs unreadable (test); a removed
             member's keys are shredded in every app of the tenant; source-mapped frames.
After:       O-1b, X-1
```

```markdown
ID:          O-4
Goal:        The post-deploy browser check (design 9.6 level 1) as the stage check of a
             deployment, run as a job workload.
May change:  crates/ruum-core/src/apps/check.rs, the headless browser runner.
Done when:   a preview with a console error, a failing request or a 500 reports them in
             AppDeployment.check with a stage error; new issues are linked; check minutes are
             emitted.
After:       W-2, O-1a
```

```markdown
ID:          O-5
Goal:        Logging helpers and the SDK for Node (`@ruum/log`, `@ruum/app`): message templates
             with named fields, the lint rule that blocks secret fields in log calls, the
             Ruum-User check; a dev mode with a local signing key and JWKS.
May change:  sdk/node/ (new folder).
Done when:   the SDK checks iss, aud, typ, env and exp against RUUM_ISSUER, RUUM_AUDIENCE and
             RUUM_ENV, refuses another app's and a preview's token, and accepts a token with an
             extra claim and an unknown actor; the lint rule finds a secret field in a log call.
             Publishing to npm is a person's step.
After:       G-1a
```

```markdown
ID:          X-1
Goal:        Issues: the provider ruum.issue, createIssue (comment), listIssues, getIssue,
             updateIssue, listIssueEvents; template validation; mutes that expire.
May change:  crates/ruum-core/src/issues/, crates/ruum-api/src/routes/issues/.
Done when:   T-4's issue cases pass; a commenter files but sets no assignee, status or links;
             filing is rate limited per principal; a mute longer than 30 days is refused; manual
             reopen works.
After:       C-3, L-1 (the gallery issue templates)
```

```markdown
ID:          X-2
Goal:        Reports, receipts and reveal requests: createIssueRevealRequest,
             resolveIssueReceipt, decideIssueReveal (actor issue_receipt); the hold and its end;
             the reporter_email deletion.
May change:  crates/ruum-core/src/issues/{reports,reveal}.rs, routes/issues/receipts.rs.
Done when:   a refused field cannot be asked again; an allowed field is decrypted only into the
             issue; agents see the pseudonym of a revealed value; the hold ends at close or after
             30 days; reporter_email goes 30 days after close; receipt tokens are stored as
             hashes.
After:       X-1, O-3
```

```markdown
ID:          L-1
Goal:        Templates: listTemplates, getTemplate, createTemplate, the gallery folder with the
             first templates (web app + Postgres + sign-in, static site, internal tool; bug,
             feature, question, error), loaded through TemplateSource; the default AGENTS.md and
             an app.yaml reference generated from the schema descriptions.
May change:  crates/ruum-core/src/templates/, routes/templates/, gallery/.
Done when:   createTemplate never copies full data, secrets, sessions, logs or report
             attachments; gallery templates are readable by every signed-in principal and never
             a node of another tenant.
After:       C-3
```

```markdown
ID:          L-2
Goal:        instantiateTemplate and getTemplateSetup (with wait): the setup steps and role
             placeholders.
May change:  crates/ruum-core/src/templates/setup.rs.
Done when:   "web app + Postgres + sign-in" makes a database, an app with a ready preview and
             issue templates in one call; guests need guests_create_resources; an agent can do
             it within its scope (the agent benchmark's M3 task).
After:       L-1, W-2, R-2b, D-1, D-3a, D-2 (step 2)
```

```markdown
ID:          V-1
Goal:        Public projects: getProjectPublication, updateProjectPublication, the anonymous
             read paths (repo reads come with step 4; database: a public schema through
             queryDatabase for anonymous callers, an additive contract change).
May change:  crates/ruum-core/src/publication/, routes/projects.rs.
Done when:   only chosen items get the tuples; personal columns are hidden markers for the
             public; issues are published without report attachments; public queries are rate
             limited and cached.
After:       restricted folders (decision 8), W-1, C-4
```

```markdown
ID:          M-2a
Goal:        The CLI for the static and node path: ruum app create|deploy|status|publish|logs
             with --wait and stage progress; the MCP smoke test of M1 (the tools appear by
             themselves as operations land).
May change:  crates/ruum-cli/, crates/ruum-mcp/tests/.
Done when:   the M1 benchmark task passes over MCP with source.kind folder; ruum app deploy
             keeps one archive file with versions per app.
After:       W-2, W-3a
```

```markdown
ID:          M-2b
Goal:        The full agent benchmark of step 3 over MCP and the remaining CLI commands (issues,
             templates).
May change:  crates/ruum-cli/, crates/ruum-mcp/, bench/agent/.
Done when:   the benchmark tasks of M1 to M4 pass over MCP against the laptop stack.
After:       M-2a, O-2, X-1, L-2
```

```markdown
ID:          U-3
Goal:        The UX package of step 3 (design 18.4.1): journeys (deploy a preview, publish,
             roll back, give an app role, read an error, file and follow a report on the report
             page, start from a template), wireframes, the copy of every new state, contract
             needs.
May change:  docs/ux/apps/ (new).
Done when:   the UX review panel accepts; contract needs are filed for C-3 (additive, within the
             named exception of the freeze).
After:       none
```

```markdown
ID:          U-4
Goal:        The web UI of step 3 (see the area), including the report page.
May change:  web/.
Done when:   the UX review panel accepts the screens; axe and visual snapshots pass.
After:       U-3 and the operations each screen needs
```

```markdown
ID:          T-5
Goal:        Harden step 3: the sandbox and telemetry suites strict, the mutation check
             (including one mutation per system lookup), two security reviewers (sandbox and
             privacy lenses), load test of the gateway, the agent benchmark with app tasks,
             synthetic users on the app pages and the report page.
After:       the build cards
```

## Order

1. **Specify** (now): C-3 review; S-2, U-3 in parallel; T-4 and T-6 after C-3; C-4 after C-3.
2. **Foundations** in parallel: R-1, O-1a, I-4 (after S-2), I-5, W-1, L-1.
3. **M1 static**: W-2 (static), G-1a (after S-2 points 2 and 4), B-2, W-3a (no migrations),
   M-2a. Static apps ship here, before the sandbox: the first usable step.
4. **M2 node**: R-2a, R-2b, G-1b, R-3, O-1b, O-2, O-4, W-4, I-6, W-3b, O-5.
5. **M3 data** (waits for step 2: D-3a, D-2, D-5): B-1 database bindings, W-2 database
   previews, L-2, W-3c (after O-2 and X-1).
6. **M4 reports and more**: X-1, O-3, X-2, G-2, G-4, W-5 (after C-4), B-3, G-3, R-5, R-6, R-4.
7. **Clients**: M-2b and U-4 as their operations land; I-7, I-8.
8. **Harden**: T-5.
9. **Later**: V-1 after restricted folders and C-4.

Migrations go through the queue: 0011 is the only migration of the step; a change during step 3
is made in place by a migration task (it is not released), as in step 2.

## Open questions

All 24 questions of `ruum-server-wt/panel/step3-open.md` have a provisional answer in
`docs/decisions/0006-step3-spec.md` (numbered as in the panel file; the short list that this page
had before is gone). Four of them are decisions for the person, written as decision briefs at the
end of 0006: the Public Suffix List request (15), plan limits (16), who the platform admins are
(17), publishing the SDKs to npm (18), plus the approval of the new binaries and dependencies
(Railpack, a Docker API client, OTLP and tonic; design 18.4).
