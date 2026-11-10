# Step 3 (Web apps): work areas, task cards and order

Status: DRAFT for the architect panel (design section 18.9), branch `s3/spec`. Nothing here is
built. The contracts are drafted and CI stays green with them: every new operation is
`x-ruum-status: planned`, with a standard request and isolation and permission cases in
`ruum-testkit`. The decisions are in `docs/decisions/0006-step3-spec.md` (`provisional`); the
open questions are in `ruum-server-wt/panel/step3-open.md` and at the end of this page.

This page splits step 3 of `docs/DESIGN.md` (section 16) into work areas and task cards for the
swarm (section 18.3). It covers design section 9 (all of it: 9.1 to 9.10), 2.7 (templates), 8.5
(agents and data, where apps touch it), 11.1 and 11.2 (where they touch apps), 17.4, 18.1 to 18.3,
19 (operations) and 21 (costs). It follows the decisions of section 17.4:

| # | Decision | What it means for this step |
|---|---|---|
| 20 | Static and Node.js first, then Python | Runtimes `static` and `node22` in the first wave; `python3.13` is a later card (R-4); `container` is not in step 3. |
| 21 | HTTP, WebSockets, scheduled jobs; uploads through the blob API | The gateway passes WebSockets; `jobs` in `app.yaml`; the runtime API reads bound files, and uploads use the blob API. |
| 22 | Public apps and custom domains only after a Ruum admin approves, per tenant | `Tenant.app_approval` (read-only); `access: public` and `addAppDomain` answer `403` (hints `public_apps_not_allowed`, `custom_domains_not_allowed`) without it; in a team tenant only owners and admins make an app public. |
| 23 | Internet through an egress proxy that logs and limits | An egress proxy on the laptop stack and in production; `egress.allow` narrows it. |
| 24 | No AI gateway in the first version | Apps call model providers through the egress proxy with their own entered secrets. |
| 25 | 1-3 s cold start is fine on cheap plans | Scale to zero (`min_instances: 0`) is the default. |
| 26 | App-level roles | `app_role` in SpiceDB; rights on objects inside an app are the app's own data. |

Step 3 also uses decisions 19 (migrations at deploy time), 30 (previews on a database branch) and
31 (tests block Publish, not the preview: step 4).

## The contracts of step 3 (drafts on `s3/spec`)

| Contract | File |
|---|---|
| App manifest (`app.yaml`) | `contracts/manifests/app.schema.json` |
| Template, issue content, telemetry privacy file | `contracts/manifests/template.schema.json`, `issue.schema.json`, `telemetry-privacy.schema.json` |
| App gateway, runtime API, user token | `contracts/apps/` (`README.md`, `gateway.openapi.yaml`, `runtime.openapi.yaml`, `user-token.schema.json`) |
| REST API | `contracts/openapi.yaml`, tags `apps`, `issues`, `templates` (45 operations, each with `x-ruum-area` and `x-ruum-status: planned`; 8 are users only) |
| MCP tools | `contracts/mcp/tools.yaml` (35 tools, not offered while planned; the 8 user-only and the 2 receipt operations are not exposed) |
| SpiceDB schema | `contracts/authz/schema.zed` (app principals, active app roles, `use`, `deploy`, `publish`, `read_telemetry`, `configure`, `app_api_scope`, `anonymous:*`), `schema.test.yaml` (31 new relationships, 32 new assertions) |
| SQL | `migrations/0011_apps.sql` (draft; the integrator renumbers it if another migration lands first) |
| Journal events | `contracts/events/` (21 step 3 types in `$defs/planned_types`) |

The step 1 and step 2 operations keep their shape. `oasdiff breaking` (CI's check, `--fail-on
ERR`) reports 0 errors; the 84 warnings are the new values of the response enum `EventType`.
Additive changes to existing schemas: `Tenant.app_approval` (read-only), `Limits.apps`,
`Usage.apps`; additive text on `copyNode` (copying a web app) and `restoreVersion` (a version of
an app is a live deployment; an agent rolls back through `publishAppDeployment`). **No new error
code**: the new refusals use existing codes with hints (`permission_denied` with
`binding_not_allowed`, `public_apps_not_allowed`, `custom_domains_not_allowed`,
`tenant_admin_required`, `publishAppDeployment`; `conflict` with `secret_missing`,
`raw_copy_gone`), so `ErrorCode` and the web copy do not change.

### Planned operations (decision 0004-21)

The rule of step 2 holds unchanged: every step 3 operation carries `x-ruum-status: planned` until
its card builds it; `matrix::PLANNED_OPERATIONS` lists them (57 with the 12 of step 2 that are not
built yet); the card that builds an operation removes its mark and its list entry, runs
`gen-tools.mjs` (its MCP tool appears), and seeds what its standard request needs in
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

The gateway paths (`/.ruum/v1/...`) and the runtime API (`ruum.internal`) are not in
`openapi.yaml`; their acceptance tests belong to cards G-1 to G-4 and R-3.

### Permission table for web app nodes (design 3.2, 9.2 to 9.4, 11.1)

| Action | Person | Agent (user's right ∩ scope, plus) |
|---|---|---|
| Use the app (through the gateway) | `use` = `view` on the app node, or any active app role | the same, through the gateway with an agent token (`actor: agent` in the user token) |
| Open a preview | `edit` | `edit` |
| Create an app | `edit` on the folder; guests need `guests_create_resources` | `edit` (scope `max_role` edit) |
| Deploy a preview | `edit` on the app; `view` on the source file; the role of every binding on its target (`binding_not_allowed`) | the same (previews never touch live data) |
| Publish, roll back | `edit`; `manage` on a database that a migration changes | **waits for a person**: `202` with an approval; the agent's user or a manager of the app decides; then the agent calls again with `approval_id` |
| Read logs, metrics, traces, error events | `edit` | `edit`; personal fields as pseudonyms, secrets as markers, log text untrusted |
| Settings (telemetry mode, widget, rollback) | `manage` | `manage` |
| `access: public` | `manage`, owner or admin of a team tenant, platform approval | never (`user_required`) |
| Enter a secret value | `manage`, users only | never |
| Rotate a generated secret | `manage` | `manage` |
| Custom domain | add: `manage`, owner or admin, platform approval, users only; remove: `manage` | remove only |
| App role grants | `share` on the app, or a holder of the `grants_roles` role | as `addGrant` |
| Partner API keys | create: `manage`, users only; list, revoke: `manage` | list, revoke |
| Confirm telemetry classes | `manage`, users only (also inside `decideAppApproval`) | suggest them in `telemetry-privacy.yaml` |
| Pseudonym of a known value | `edit`, users only, rate limited | never |
| Issues | file: `edit` on the folder (open question 6); read: `view`; change: `edit`; events and report attachments: `edit` | the same |
| Ask a reporter to reveal fields | `edit`, users only | never in step 3 |
| Templates | instantiate: `view` on the template (gallery: anyone signed in) and `edit` on the parent | the same |
| Public project | `manage` on folder and items, owner or admin, platform approval, users only | never |

The order of checks for an agent's publish: the user's right and the scope (`edit`), the
deployer check of every binding (live roles), the secrets that must be set, then the approval
(`202`), then (with `approval_id`) every check again.

## Shared files (not owned by any area)

As in steps 1 and 2, plus:

| Path | What |
|---|---|
| `contracts/manifests/`, `contracts/apps/` | Contract changes only. |
| `crates/ruum-core/src/apps/mod.rs` | Made by card W-1 with the module tree (`deployments`, `bindings`, `roles`, `secrets`, `domains`, `apis`, `approvals`, `settings`) as empty files, so parallel cards edit only their file (as D-1 did, decision 0004-33). |
| `crates/ruum-api/src/routes/apps/mod.rs`, `routes/issues/mod.rs`, `routes/templates/mod.rs` | Made by W-1, X-1 and L-1 with `501` stubs for every handler of their tags and one submodule per card. |
| `crates/ruum-core/src/runtime/mod.rs` | The `RuntimeDriver` trait (module border, design 18.1): made by R-1. Signature changes need the agreement of the `runtime` and `apps` areas. |
| `crates/ruum-core/src/telemetry/mod.rs` | The `TelemetryStore` and `TelemetryIngest` traits (module border): made by O-1. |
| `crates/ruum-core/src/privacy/mod.rs` | Step 2's marker types and `Pseudonymizer`; the telemetry classes use them (O-2). Changes need the agreement of the `query` area of step 2. |

Planned marks and generated files: as in step 2 (the integrator keeps both removals and runs
`gen-tools.mjs` and `npm run gen:api` again).

## How an app runs (the security model)

AI-written code is untrusted code (design 9.5). Seven rules, each with a test:

1. **Isolation.** Production: Kubernetes with the gVisor runtime class in a separate node pool,
   Cilium network policies, scale to zero (Knative Serving or the KEDA HTTP add-on). Laptop: the
   same `RuntimeDriver` trait with a **Docker driver** (one container per instance, no
   privileges, read-only root, a user namespace, seccomp default, no Docker socket, CPU and memory
   limits, one Docker network per app), and an optional **runsc driver** (Docker with the gVisor
   runtime `runsc`) on Linux hosts such as the build server. One conformance suite runs against
   every driver (R-1, R-2). gVisor in Kubernetes and scale to zero are tested on staging (design
   12.2); until staging exists, the runsc driver on the build server is the closest check.
2. **Network: default deny.** An app sandbox reaches the internal gateway (`ruum.internal`), its
   bound databases (the data cluster, with `pg_hba.conf` lines for the app role kinds from the
   pool's addresses, as STEP2 "Infrastructure work" 2 foresaw), the apps it binds to, and the
   egress proxy. Never the platform database, SpiceDB, OpenBao, NATS, Garage, the metadata
   endpoint or another tenant's sandbox. A hostile test (`R-2`) tries each from inside a sandbox.
3. **The gateway checks every request** (contracts/apps/gateway.openapi.yaml): `use` for live,
   `edit` for previews, strips `Ruum-*` headers, adds a fresh `Ruum-User` token (at most 60 s,
   EdDSA, a key set apart from the API keys). The app gets no platform cookie: the apps domain is
   another registered domain, every app origin is its own site (`*.ruumapps.net` on the Public
   Suffix List), and the gateway's own session cookie is `__Host-` and per origin.
4. **No privilege escalation through bindings.** The deployer must hold every binding's role on
   its target (and the right to grant API scopes); for an agent, its user's right ∩ its scope.
   The grant records `granted_by` (decision 0006-3). An app never gets `manage` on a node.
5. **Previews never touch live data** (decision 30, 0006-5): a preview is a principal of its own;
   its database bindings go to masked branches; other bindings are capped at `view`; entered
   secrets reach a preview only when the manifest says `preview: true`.
6. **Secrets are write-only** (design 9.9): `generated` (most), `entered` (a person pastes once);
   the values live only in OpenBao (KV v2, one path per app and name); the running app gets them
   as environment variables at start; no operation, log, journal entry or export holds a value;
   there is never a database password (bindings with short-lived roles).
7. **An agent's publish waits for a person** (design 11.1, decision 0006-4): an agent can make
   previews freely and ask to publish; the person decides with the preview, the check result and
   the changes in front of them, and confirms new telemetry fields and columns in the same step.

### Deploy, publish, roll back

```
agent / person           platform (apps area)                    runtime / gateway
  | createAppDeployment   |                                        |
  |---------------------->| unpack (limits) -> validate (schema,   |
  |                       | provider rules) -> bindings (resolve,  |
  |<-- 202 (preview) -----| deployer check, branches for dbs)      |
  |                       | -> migrations (on the branches)        |
  |                       | -> image (static to Garage; node22:    |
  |                       |    buildpacks -> OCI image) ---------->| start (driver)
  |                       | -> check (health, headless browser) <--|
  | getAppDeployment      | state ready, check result              |
  | publishAppDeployment  | person: go on; agent: 202 approval     |
  |---------------------->| live bindings, migrations on live      |
  |                       | (restore point, one transaction each), |
  |                       | new version (app_release_v1) --------->| start live, check, switch
  |                       |                                        | traffic; old one stops
  |                       | auto rollback if the error rate rises  |
```

A version of a web app node is one **live** deployment (encoding `app_release_v1`: the archive's
chunk list and the parsed manifest; decision 0006-2), so `listVersions` is the release history and
`restoreVersion` is a rollback (people; agents use `publishAppDeployment`). Previews are rows of
`app_deployments` only; their archives stay in the blob store while the preview exists. A rollback
across a migration needs `force` and leaves the database as it is (forward-only migrations; open
question 9).

The **automatic rollback** (design 9.6, layer 1 of design 19.1: no agent in the loop) compares
the error rate in the 15 minutes after a publish with the hour before; above
`auto_rollback.error_rate_factor` (default 3) and above 1 %, it publishes the previous live
deployment and writes `app.rolled_back` (reason `error_rate`), then tells the session (step 5)
and opens an issue.

## Telemetry and privacy (design 9.6)

```
app stdout ──> runtime log shipper ─┐
browser script ─> gateway /.ruum/v1/telemetry ─┤
OTel SDK (runtime adds it) ─────────┼─> OpenTelemetry Collector ──> Loki / Mimir / Tempo
build and deploy events ────────────┘   processors: tenant label,     (one storage tenant per
                                        classes from telemetry_fields, Ruum tenant)
                                        secret -> hidden marker,
                                        personal -> keyed pseudonym,
                                        raw copy (optional) -> encrypted per end user
```

- **Classes** follow 8.5: `secret` is never recorded (the hidden-value marker, at the source by the
  logging helpers and again at ingestion); `personal` becomes a keyed pseudonym (HMAC with the
  tenant key, the same `Pseudonymizer` as step 2, so `computeAppPseudonym` finds a person's
  records); `normal` is readable. Unclassified fields count as `personal`. Metric labels may only
  be `normal` (validation of `telemetry-privacy.yaml`, and ingestion drops the label). Free text
  without a template is `personal` and scrubbed (patterns, then name recognition: open question
  12).
- **The raw copy** (off by default, `raw_copy_days` up to 30): each end user's raw `personal`
  values are encrypted under their own key (`telemetry_end_user_keys`, wrapped by the tenant key);
  deleting the key shreds them. In step 3 a raw value is decrypted only into an issue, by the
  reporter's own choice (`include`) or answer to a reveal request. A grant to read raw copies
  outside reports is not in step 3 (open question 11).
- **Access**: `edit` on the app; every API answer marks log text as untrusted.
- **Errors reach agents without being asked** (design 9.6): level 1, the post-deploy check, is in
  `createAppDeployment` (stage `check`, card O-4); level 2 (push to the active session) and level 3
  (the guardian) need agent sessions (step 5). In step 3 the issues and the journal events
  (`issue.created`, `issue.status_changed`) are the hooks that step 5 subscribes to.
- **Source maps** come in the archive (`telemetry.source_maps`), are stored privately and never
  served; the error pipeline maps frames before storage.
- **Retention** by plan (`AppPlanLimits.telemetry_days`); the storage tenant of a Ruum tenant is
  deleted with the tenant.

## Issues and reports (design 9.7)

An issue is a typed node of type `issue` (provider `ruum.issue`; decision 0006-6: the design text
says "a file of type `issue`"; a typed node keeps the content validated against its template and
lets counts live in provider state without making versions). Its content
(`contracts/manifests/issue.schema.json`) is its versions; comments are node comments; labels are
tags; a board is a smart folder grouped by status.

- **One concept for problems**: people, agents, monitoring and the report widget make the same
  kind of issue (`source`). The first occurrence of a fingerprint opens an issue
  (`issues_fingerprint_idx`), later ones count. A closed issue whose error returns becomes
  `regressed`. A mute always expires (default 7 days, at most 30). Many new fingerprints from one
  publish are grouped under one parent issue (`links.deployment_id`).
- **Reports**: the widget sends to the gateway (`/.ruum/v1/reports`); the gateway attaches the
  records of the visit (linked by the visit's trace IDs, only the visitor's own), applies the
  reporter's choices per `personal` field (`include` / `pseudonymise` / `remove`), holds the raw
  copies of the visit until the issue closes (at most 30 days), and makes the issue from the
  app's bug template. Non-members get a receipt token (`/r#<token>` on the platform origin;
  `resolveIssueReceipt`, `decideIssueReveal`). Public apps rate-limit reports per visitor and app.
- **Fix in a session** (a button that opens an agent session) is step 5; step 3 keeps
  `links.session_ids` in the content for it.

## Templates (design 2.7)

- A template is a folder of type `template` with `template.yaml`. Kinds: `project` and `issue`.
- **The gallery** is bundled with the platform (`gallery:<slug>`, versioned with the release),
  never a node of another tenant, so no operation reads two tenants. The first gallery: `web app +
  Postgres + sign-in`, `static site`, `internal tool`, and the issue templates `bug`, `feature`,
  `question`, `error` (the template of monitoring issues).
- **Setup** (`instantiateTemplate`): folders, databases (manifest, migrations, masked sample data),
  the app from its archive with the first preview, issue templates, role placeholders. Step 4
  replaces the archive with a repo scaffold. "Update from the template" (diff and merge) needs
  step 4's merge plugins: backlog.
- `createTemplate` follows the copy rules of typed nodes: databases as masked sample data (never
  `full`), apps without secrets; sessions, agent memory, logs and report data are never in a
  template.

## Public projects (design 9.9)

Specified now, built **after restricted folders** (design 16, decision 8):
`updateProjectPublication` writes `viewer@anonymous:*` or `viewer@user:*` on each chosen item,
never on the folder. A database item needs a public schema; `personal` columns are the hidden
marker for the public (no pseudonyms: outsiders could link records); public queries have a short
timeout, a row cap, a rate limit per visitor and caching. The extension of `queryDatabase` for
anonymous callers is a later additive contract change (card V-1). Remix stays in the backlog.

## Apps calling apps (design 9.10)

- **API bindings** (`bindings.<name>.api` + `scopes`) give the calling app's principal
  `app_api_scope:<called>/<api>/<scope>#consumer`. The deployer must have `grant` on each scope
  (`manage` on the called app, or the app role named in `granted_by`).
- **Internal** calls go to `http://<binding>.<api>.ruum.internal` through the internal gateway,
  which adds `Ruum-User` with `actor: app`, the scopes, and for a delegated call `on_behalf_of`
  and the `chain` (at most 3 hops). Cilium allows a sandbox to reach another only along a
  binding.
- **External** APIs are published through the app gateway for users with app roles, agents (MCP
  publication of an app's OpenAPI is backlog), partners with API keys (`createAppApiKey`), or
  the public (with platform approval).
- **Events**: an app publishes declared topics through the runtime API; subscribers get them at
  least once at a path, with retries, a dead-letter queue and replay (NATS JetStream). Ruum's own
  events for nodes in a binding's scope come the same way.
- **Contracts between apps**: the OpenAPI or AsyncAPI file is in the archive; the deploy warns when
  a change breaks a bound consumer (the blocking check is part of step 5's pipeline, 11.2).
- Calls between tenants only through external APIs.

## Agents and data (8.5), workspaces (11.1, 11.2)

- An app's database bindings use the app role kinds that step 2 reserved (decision 0004-32): the
  app principal gets a LOGIN role per binding, short-lived password, pooled; the privacy filter
  does not apply to the app itself (it is the owner's code with the owner's grant), but every
  agent that reads the same database through the query API still gets the lens.
- Previews get masked branches (decision 30), so a preview never copies real personal data.
- Migrations in `app.yaml` carry their column classes (`-- ruum:classes`); the publish approval
  lists unconfirmed columns and telemetry fields in one place.
- Step 3 builds what step 5's sessions need: previews, the check result, issues and their events,
  telemetry by API and MCP. The session loop, the guardian and the pipeline of 11.2 are step 5.

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
| `telemetry.delete_end_user` (crypto-shred) | amber | GDPR erasure, design 9.6 |

Runbooks: a sandbox escape suspicion, an egress abuse, a phishing report against a public app, a
failing certificate renewal. Game days on staging (19.5) add "an app floods the egress proxy"
and "a preview tries to reach the live database".

## Costs and metering (design 21)

| Meter | Level (21.2) | Source |
|---|---|---|
| instance seconds and memory of app sandboxes | counters per app, hourly | runtime driver |
| requests through the gateway | counters per app, hourly | gateway |
| egress bytes | counters per app, hourly | egress proxy |
| image and static storage | daily snapshot | blob store |
| telemetry bytes ingested and stored | counters per app, daily snapshot | collector, Loki/Mimir/Tempo |
| deployments, checks (headless browser minutes) | each event | apps area |
| custom domains (certificates) | each event | gateway |

Every usage event carries tenant, app node, and `actor` / `on_behalf_of` of the deploy that
caused it (design 21.2). The plan limits of `AppPlanLimits` are placeholders until the cost agent
has staging costs (21.4). Card I-8 adds the meters; prices are a decision of the person.

## The areas

### 1. Web apps (control plane)

| | |
|---|---|
| Owns | `crates/ruum-core/src/apps/` (`mod`, `deployments`, `settings`, `secrets`, `domains`, `approvals`, `archive`), `crates/ruum-api/src/routes/apps/` (except `roles.rs`, `apis.rs`, `telemetry.rs`) |
| Implements (OpenAPI) | area `apps` |
| Tables | `apps`, `app_deployments`, `app_secrets`, `app_domains`, `app_approvals`, `tenant_app_settings` |
| Notes | The provider `ruum.webapp` implements `providers::Provider` (create, copy, trash = suspend, restore, purge = stop and delete images; `copied` makes a preview from the live archive). Manifest validation: the JSON Schema compiled in, plus the provider rules (plan limits, `env` vs `secrets`, one `default` role, one `grants_roles`, binding targets resolvable). Secrets through a `SecretStore` trait (OpenBao KV v2; in-memory for tests). |

### 2. Bindings, roles and APIs

| | |
|---|---|
| Owns | `crates/ruum-core/src/apps/{bindings,roles,apis}.rs`, `crates/ruum-api/src/routes/apps/{roles,apis}.rs` |
| Implements (OpenAPI) | areas `roles`, `apis` |
| Tables | `app_bindings`, `app_role_grants`, `app_api_keys` |
| Contract | `schema.zed`: `app`, `app_role` (`active`), `node#active_role`, `use`, `app_api_scope` |
| Notes | The deployer check; the tuples of bindings (live and preview principals); active roles follow the live manifest; database bindings through the app role kinds of `database_roles` (with the databases area of step 2: `RoleRef` with a binding key, decision 0004-32). |

### 3. Runtime

| | |
|---|---|
| Owns | `crates/ruum-core/src/runtime/` (the `RuntimeDriver` trait, the in-memory driver, the conformance suite), `crates/ruum-runtime/` (new crate: the Docker driver, the runsc driver, the image builder), `crates/ruum-runtime-api/` (new crate: the runtime API server at `ruum.internal`, `contracts/apps/runtime.openapi.yaml`) |
| Notes | Build: static files go to Garage as they are; `node22` archives go through a builder (Railpack or Cloud Native Buildpacks; open question 3) into an OCI image in a local registry. Start, stop, scale to zero (an idle timer in the driver on the laptop), logs to the collector, health. Workload tokens mounted and renewed. Egress only through the proxy. |

### 4. Gateway

| | |
|---|---|
| Owns | `crates/ruum-gateway/` (new crate, a binary of its own; decision 0006-1), `deploy/dev/caddy/` changes for `*.ruumapps.localhost` |
| Implements | `contracts/apps/gateway.openapi.yaml`, `user-token.schema.json` |
| Notes | Host to app and deployment lookup (a cache fed by the journal), the `use` check (SpiceDB, with a few seconds of cache per visitor and app), the app session (`__Host-ruum_app`, sign-in through the platform), header stripping and the user token, the browser script and the report widget endpoints, WebSockets, rate limits, partner API keys, the internal gateway for app-to-app calls, custom domains with ACME (production; the laptop uses Caddy's local CA). Scale to zero wakes the app through the driver and holds the request. |

### 5. Telemetry

| | |
|---|---|
| Owns | `crates/ruum-core/src/telemetry/` (traits, the in-memory store, classes, the pseudonym step), `crates/ruum-api/src/routes/apps/telemetry.rs`, `deploy/dev/otel/` (collector config) |
| Implements (OpenAPI) | area `telemetry` |
| Tables | `telemetry_fields`, `telemetry_end_user_keys` |
| Notes | The collector processors driven by `telemetry_fields` (a small Rust processor service or the OTel transform processor fed by a generated config; open question 4). The logging helpers for Node (`@ruum/log`: message templates with named fields) and the SDK (`@ruum/app`: checks `Ruum-User`), published to npm later (a person publishes). Error grouping (fingerprints), source maps, the post-deploy check (Playwright on the build host). |

### 6. Issues

| | |
|---|---|
| Owns | `crates/ruum-core/src/issues/`, `crates/ruum-api/src/routes/issues/` |
| Implements (OpenAPI) | area `issues` |
| Tables | `issues`, `issue_reports`, `issue_reveal_requests` |
| Notes | The provider `ruum.issue`; template validation (JSON Schema of the template version); counts from the telemetry error stream; regressions; mutes that expire; reports from the gateway; receipts. |

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
| Notes | See "Infrastructure work" below. |

### 9. MCP and CLI

| | |
|---|---|
| Owns | `crates/ruum-mcp/`, `crates/ruum-cli/` |
| Implements | The 35 tools of `contracts/mcp/tools.yaml` as their operations land; `ruum app create|deploy|status|publish|logs|issues`, `ruum template ls|new`. `ruum app deploy <dir>` packs the folder into an archive, uploads it as a file node and calls `createAppDeployment`. Log lines and issue text are untrusted data in every tool result. |

### 10. Web UI

| | |
|---|---|
| Owns | `web/` except `web/src/api/schema.d.ts` |
| Implements | The app page (status, address, deployments with Publish and Rollback, previews, settings, secrets with "enter value", domains, roles in the share dialog, API keys), the logs and metrics views, telemetry classes in the publish approval, the issue list and board, the issue page, "New project from template", the receipt page (`/r#token`). UX package first (card U-3). |

### 11. Suites (test harness)

| | |
|---|---|
| Owns | `crates/ruum-testkit/` |
| Implements | Isolation and permission cases for the 45 operations (they exist on `s3/spec`, reported as planned); the fixture seeding of the app, issue and template (each card seeds what its operations need); a **sandbox suite** (hostile apps: reach the platform database, another tenant's app, the metadata endpoint, OpenBao; send `Ruum-User` from outside; replay a user token at another app; read another app's secrets); a **telemetry privacy suite** (no raw `secret` value is ever stored; no raw `personal` value reaches a reader without a report choice; metric labels with personal values are dropped); mutations for each rule. |

## Infrastructure work (laptop stack)

1. **The app gateway** as a compose service built from `crates/ruum-gateway` (or `cargo run -p
   ruum-gateway` beside the server), with Caddy's `*.ruumapps.localhost` block proxying to it
   (today it answers `503 not built yet`). The internal gateway listens on the app network as
   `ruum.internal`.
2. **The runtime on the laptop**: the Docker driver talks to the Docker engine through a
   **socket proxy** that allows only container, image and network calls under the label
   `ruum.app` (never the raw socket in the server). One Docker network per tenant app,
   `internal: true`, with the gateway, the runtime API, the egress proxy and the data cluster
   attached. Optional: the `runsc` runtime (gVisor) where the host is Linux (the build server);
   `dev.sh env` prints `RUUM_RUNTIME_DRIVER=docker|runsc`.
3. **A local OCI registry** (`registry:2` on 127.0.0.1:5001) for app images, and the builder
   (Railpack or `pack` with a pinned builder image).
4. **The egress proxy**: a small forward proxy (Envoy or Squid; open question 5) that logs host,
   bytes and the app, enforces `egress.allow`, and refuses private address ranges (the platform's
   own services, the metadata address 169.254.169.254).
5. **Telemetry stack**: the OpenTelemetry Collector plus Loki, Mimir (or Prometheus on the
   laptop) and Tempo; the single-container `grafana/otel-lgtm` image is enough on the laptop
   (with its pinned version), with multi-tenancy headers. Production uses the design's Grafana
   stack with one storage tenant per Ruum tenant.
6. **OpenBao KV v2** for app secrets (OpenBao already runs in dev mode on the laptop; secrets of
   the laptop are lost on restart, which is fine for dev; `dev.sh seed` sets the fixture's).
7. **NATS JetStream** streams for app events (NATS already runs).
8. **A headless browser** for the post-deploy check: Playwright's Chromium (the build server has
   it for `e2e_browser.sh`).
9. **CI**: a job that runs the sandbox suite with the Docker driver (GitHub's runners have
   Docker; runsc can be installed there too), `redocly lint` for `contracts/apps/*.openapi.yaml`,
   `ajv` for every manifest's `examples`.
10. **Staging**: gVisor runtime class, Cilium policies, Knative or KEDA scale to zero; not before
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
`orders` (`orders.read`, `orders.write`) and a binding `db` with role `edit` on D.

## Task cards

IDs: `C` contract, `S` spike, `I` infrastructure, `W` web apps (control plane), `B` bindings, roles
and APIs, `R` runtime, `G` gateway, `O` observability (telemetry), `X` issues, `L` templates, `V`
public projects, `M` MCP and CLI, `U` UI, `T` tests. Numbers continue the step 2 numbers where
the letter is shared (C, S, I, M, U, T). "After" lists the cards that must be merged first. Every
card that builds an operation also removes its planned mark and its `PLANNED_OPERATIONS` entry,
regenerates `tools.json`, and seeds its fixture fields (not listed again under "May change").

```markdown
ID:          C-3
Goal:        Review and freeze the step 3 contracts of s3/spec (this draft).
Contracts:   contracts/manifests/{app,template,issue,telemetry-privacy}.schema.json,
             contracts/apps/*, the tags apps, issues, templates of openapi.yaml, schema.zed,
             mcp/tools.yaml, migrations/0011_apps.sql, the step 3 event schemas.
May change:  those files and the generated files; crates/ruum-testkit/ (the lists, the fixture
             fields, standard requests, isolation cases, permission rules, permission_matrix.md);
             crates/ruum-mcp/tests/content_trust.rs (UNTRUSTED_FIELDS); docs/STEP3.md;
             docs/decisions/0006-step3-spec.md.
Must not:    product code.
Done when:   the architect panel accepts (two reviewers, one with the security lens);
             redocly (openapi.yaml and contracts/apps/*.openapi.yaml), zed validate, oasdiff
             (0 errors), the migration applies on a scratch database, ajv for the manifest,
             token and event examples; cargo test --workspace and RUUM_REQUIRE_IMPLEMENTED=1
             cargo test -p ruum-testkit (memory and postgres) pass; the open questions of
             panel/step3-open.md have a provisional answer in 0006.
After:       none (the review can start now)
Out of scope: building anything.
```

```markdown
ID:          S-2
Goal:        Spike: the sandbox and the gateway on the build server, before R-1 and G-1 freeze.
Contracts:   docs/STEP3.md "How an app runs"; contracts/apps/.
May change:  spikes/step3-sandbox/ (scripts and a short README), docs/decisions/0006 (result).
Must not:    crates/, contracts (a contract change the result needs goes to C-3).
Done when:   on the build server, a script shows: (1) a Node app in Docker with runsc (gVisor)
             and in plain Docker with the driver's flags cannot reach the platform Postgres,
             SpiceDB, OpenBao, the metadata address, the Docker socket or another app's network,
             and reaches the internet only through the egress proxy; (2) a Caddy or small Rust
             proxy in front of it strips an incoming Ruum-User header and adds a signed one;
             (3) a cold start from a stopped container answers within 3 s for a small Node app
             (decision 25), with numbers for runsc and plain Docker; (4) a static archive served
             from Garage through the proxy. If a point fails, the result names the change.
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
             preview on a masked branch, agent publish waits, secrets never in any answer, app
             role grants and the active flag, telemetry classes, mutes that expire, reveal
             requests, gallery templates); all report planned on main; a person or the panel
             reviews them (design 18.2).
After:       C-3
Out of scope: the browser check (O-4 brings its own tests).
```

```markdown
ID:          I-4
Goal:        The runtime on the laptop: Docker socket proxy, app networks, local registry,
             builder image, egress proxy; runsc on the build server.
Contracts:   docs/STEP3.md "Infrastructure work" 2 to 4.
May change:  deploy/dev/, scripts/dev.sh, docs/DEVSTACK.md, scripts/buildbox/ (setup only).
Must not:    crates/.
Done when:   `dev.sh up` starts the socket proxy, the registry and the egress proxy; `dev.sh env`
             prints RUUM_RUNTIME_DRIVER, RUUM_DOCKER_PROXY, RUUM_REGISTRY, RUUM_EGRESS_PROXY;
             a check script (`scripts/sandbox_check.sh`) runs a hostile container on an app network
             and shows that every forbidden address of R-2 is unreachable; the build server has
             runsc registered (a person runs the setup if it needs root, decision 18.9).
After:       S-2
Out of scope: Kubernetes.
```

```markdown
ID:          I-5
Goal:        The telemetry stack on the laptop: OTel Collector and Loki/Mimir/Tempo (or the
             single grafana/otel-lgtm container), with per-tenant headers.
May change:  deploy/dev/, deploy/dev/otel/ (collector config skeleton), scripts/dev.sh,
             docs/DEVSTACK.md.
Done when:   `dev.sh up` starts it; a test record with a tenant header is readable only with
             that header; `dev.sh env` prints RUUM_OTLP_ENDPOINT and the query URLs.
After:       none
```

```markdown
ID:          I-6
Goal:        The app gateway and the internal gateway in the laptop stack (Caddy wiring, the
             ruum.internal name on app networks), and CI jobs for the sandbox suite and the
             contract lints of contracts/apps.
May change:  deploy/dev/caddy/, deploy/dev/docker-compose.yml, .github/workflows/ci.yml (new
             jobs only), docs/DEVSTACK.md.
Done when:   https://<app>.<tenant>.ruumapps.localhost reaches the gateway; the CI jobs run
             (planned cases pass).
After:       I-4, G-1
```

```markdown
ID:          I-7
Goal:        Operations: the app action types and runbooks of docs/STEP3.md "Operations" as a
             proposal to the signed policy (never in force without the person).
May change:  docs/ops/ (proposal file and runbooks only).
Done when:   the guard review of the ops area accepts the proposal; the person's approval is
             listed as pending in the digest.
After:       W-3
```

```markdown
ID:          I-8
Goal:        Metering of apps (design 21): the meters of docs/STEP3.md "Costs and metering".
May change:  crates/ruum-core/src/metering/ (new) or the existing usage module, the hooks in
             runtime, gateway and telemetry (one line each, agreed with those areas).
Done when:   usage events carry tenant, app, actor and on_behalf_of; Usage.apps is filled;
             the cost of measuring stays a counter per app per hour.
After:       R-3, G-2, O-1
```

```markdown
ID:          W-1
Goal:        The web app provider and the control plane skeleton: createApp, getApp,
             updateAppSettings, listAppDeployments, getAppDeployment; the module tree with 501
             stubs for every handler of the tag apps.
Contracts:   openapi.yaml area apps; contracts/manifests/app.schema.json; 0011 (apps,
             app_deployments, tenant_app_settings).
May change:  crates/ruum-core/src/apps/{mod,settings}.rs, the empty files of the other
             submodules, crates/ruum-api/src/routes/apps/mod.rs, crates/ruum-core/src/services.rs
             (one line), crates/ruum-core/src/providers/ (registering ruum.webapp only).
Done when:   T-4's cases of these operations pass in memory and Postgres; access public refused
             without the tenant approval and for members of a team tenant; copy and trash hooks
             of ruum.webapp (copy = new app with a preview, trash = suspended).
After:       C-3
```

```markdown
ID:          W-2
Goal:        Deployments: createAppDeployment (archive from a file node, unpack with limits,
             validate, the stages, previews with expiry), through the RuntimeDriver trait.
May change:  crates/ruum-core/src/apps/{deployments,archive}.rs, routes/apps/deployments.rs.
Done when:   T-4's deployment cases pass with the in-memory driver; zip and tar slip, links,
             device files and archive bombs are refused (hostile tests); a preview binds a
             masked branch (with D-3) and caps other bindings at view.
After:       W-1, R-1, B-1, D-3 (step 2: masked branches)
```

```markdown
ID:          W-3
Goal:        Publish and roll back: publishAppDeployment, restoreVersion of an app version
             (people), app_release_v1 versions, migrations at publish (decision 19), the
             approvals (listAppApprovals, getAppApproval, decideAppApproval), automatic rollback.
May change:  crates/ruum-core/src/apps/{deployments,approvals}.rs, routes/apps/{deployments,
             approvals}.rs, crates/ruum-api/src/routes/versions.rs (the app branch of
             restoreVersion only).
Done when:   an agent's publish answers 202 and runs only with an approved approval_id, once,
             within 15 minutes; a failed migration leaves the live deployment and the database
             as they were; the automatic rollback fires on a scripted error burst; mutations
             for "an agent publishes without approval" and "an approval is used twice".
After:       W-2, D-2 (step 2: migrations)
```

```markdown
ID:          W-4
Goal:        Secrets: listAppSecrets, setAppSecret, rotateAppSecret; the SecretStore trait
             (OpenBao KV v2 and in-memory); generated secrets on deploy; scheduled rotation.
May change:  crates/ruum-core/src/apps/secrets.rs, routes/apps/secrets.rs.
Done when:   no answer, log line, journal entry, audit entry or export holds a value (a test
             greps every output of the suites for the fixture values); previews get entered
             secrets only with preview: true; the running app gets the values at start.
After:       W-1, R-1
```

```markdown
ID:          W-5
Goal:        Custom domains: listAppDomains, addAppDomain, removeAppDomain; DNS checks; ACME in
             the gateway (production), Caddy's local CA on the laptop.
May change:  crates/ruum-core/src/apps/domains.rs, routes/apps/domains.rs, the domain part of
             crates/ruum-gateway (agreed with G).
Done when:   a host under a Ruum domain or of another tenant is refused (409, no tenant named);
             without the approval 403 custom_domains_not_allowed; the gateway serves an active
             domain.
After:       W-1, G-1
```

```markdown
ID:          B-1
Goal:        Bindings: resolution of paths and IDs at deploy time, the deployer check, the tuples
             of live and preview principals, app_bindings, the end of bindings.
May change:  crates/ruum-core/src/apps/bindings.rs; crates/ruum-core/src/databases/ (the app
             role kinds of a binding, through RoleRef, agreed with the databases area).
Done when:   a deployer without the role fails at stage bindings (binding_not_allowed); an
             agent's binding is checked against its user's right ∩ scope; a dropped binding
             removes its tuple and writes app.binding_revoked; a preview never gets a live
             database; mutations for each rule.
After:       C-3, W-1
```

```markdown
ID:          B-2
Goal:        App roles: listAppRoles, listAppRoleGrants, addAppRoleGrant, removeAppRoleGrant;
             the active flag follows the live manifest; the gateway's roles claim.
May change:  crates/ruum-core/src/apps/roles.rs, routes/apps/roles.rs.
Done when:   a role removed by a new version keeps its grants and gives nothing; a rollback
             makes it active again; a grants_roles holder grants; the default role is given to
             viewers without an explicit role.
After:       W-1
```

```markdown
ID:          B-3
Goal:        APIs between apps: API bindings and scopes (app_api_scope), the internal gateway
             headers and the delegation chain (with G-3), partner API keys (listAppApiKeys,
             createAppApiKey, revokeAppApiKey), events through NATS (with R-3).
May change:  crates/ruum-core/src/apps/apis.rs, routes/apps/apis.rs.
Done when:   a call without a binding is refused at the network and at the gateway; a chain
             longer than 3 hops is refused; a partner key works only for its API, scopes and
             rate; a revoked key stops within seconds.
After:       B-1, G-3
```

```markdown
ID:          R-1
Goal:        The RuntimeDriver trait, the in-memory driver and the conformance suite.
May change:  crates/ruum-core/src/runtime/.
Done when:   the suite covers build, start, stop, scale to zero and wake, logs, health, limits
             and the network rules as observable by the driver; the in-memory driver passes.
After:       C-3
```

```markdown
ID:          R-2
Goal:        The Docker driver and the runsc driver (crates/ruum-runtime), the image builder for
             node22, static archives to Garage; the hostile sandbox suite.
May change:  crates/ruum-runtime/ (new crate; new dependencies need the person's approval:
             a Docker API client such as bollard).
Done when:   both drivers pass the conformance suite on the build server; the sandbox suite
             passes (every forbidden address unreachable; no privilege; read-only root);
             mutations that remove each driver flag fail a test.
After:       R-1, I-4, S-2
```

```markdown
ID:          R-3
Goal:        The runtime API at ruum.internal (crates/ruum-runtime-api): getAppSelf,
             listAppSelfUsers, exchangeAppUserToken, readBoundContent, publishAppEvent; workload
             tokens.
May change:  crates/ruum-runtime-api/ (new crate).
Done when:   contracts/apps/runtime.openapi.yaml is served; an app token is refused by the REST
             API and an API token by the runtime API; the exchange gives the user's right ∩ the
             binding.
After:       R-1, B-1
```

```markdown
ID:          R-4
Goal:        The python3.13 runtime (decision 20, "then Python").
May change:  crates/ruum-runtime/ (builder), the manifest provider rule that accepts it.
Done when:   a Python app passes the deployment cases; app.schema.json already lists it.
After:       R-2
```

```markdown
ID:          G-1
Goal:        The app gateway: host lookup, the use/edit check, app sessions and sign-in on the
             app origin, header stripping, the Ruum-User token, WebSockets, previews, status
             pages, scale-to-zero wake.
May change:  crates/ruum-gateway/ (new crate), crates/ruum-core/src/identity/ (an app audience for
             the code flow, agreed with the identity area).
Done when:   gateway.openapi.yaml auth paths work; a request without `use` never reaches the
             app; an incoming Ruum-User is always removed; a token for app A is refused by app
             B's SDK; a removed role stops working within 5 s; the gateway suite passes.
After:       C-3, S-2
```

```markdown
ID:          G-2
Goal:        The browser script and frontend telemetry (/.ruum/v1/script.js, /telemetry):
             privacy mode by default, full mode by setting; the report widget UI script.
May change:  crates/ruum-gateway/src/telemetry.rs, gateway/browser/ (the script source, built
             to one file).
Done when:   in privacy mode no query string, IP, cookie, body or form value leaves the browser
             or the gateway (a test inspects the stored records); events are rate limited.
After:       G-1, O-1
```

```markdown
ID:          G-3
Goal:        The internal gateway for app-to-app calls and partner keys at the edge.
May change:  crates/ruum-gateway/src/internal.rs.
Done when:   with B-3's cases.
After:       G-1
```

```markdown
ID:          G-4
Goal:        Reports: /.ruum/v1/reports and /reports/preview (the visit's records, the choices,
             the raw copy hold, the receipt token).
May change:  crates/ruum-gateway/src/reports.rs.
Done when:   only the visitor's own records are offered; include works only with a raw copy and
             only into the issue; non-members get a receipt token once; public apps are rate
             limited.
After:       G-1, O-3, X-2
```

```markdown
ID:          O-1
Goal:        Telemetry ingestion and store: the traits, the in-memory store, the collector
             pipeline with the classes (secret -> marker, personal -> pseudonym), the storage
             tenant per Ruum tenant.
May change:  crates/ruum-core/src/telemetry/, deploy/dev/otel/.
Done when:   the telemetry privacy suite passes for logs, metrics and traces: no raw secret is
             stored, personal values are pseudonyms, unclassified is personal, personal metric
             labels are dropped; mutations for each.
After:       C-3, I-5, Q-4 (step 2: tenant keys)
```

```markdown
ID:          O-2
Goal:        Telemetry classes and reads: listAppTelemetryFields, confirmAppTelemetryFields,
             computeAppPseudonym, searchAppLogs, getAppMetrics, getAppTrace,
             compareAppDeployment; telemetry-privacy.yaml from the archive.
May change:  crates/ruum-core/src/telemetry/{classes,query}.rs, routes/apps/telemetry.rs.
Done when:   T-4's telemetry cases pass; log answers carry content_trust untrusted; a trace
             shows another app's spans only with edit on that app.
After:       O-1, W-2
```

```markdown
ID:          O-3
Goal:        Errors and the raw copy: fingerprints, error groups into issues (with X-1),
             source maps, the per-end-user keys and crypto-shredding.
May change:  crates/ruum-core/src/telemetry/{errors,raw_copy}.rs.
Done when:   one fingerprint makes one issue and counts; a deleted end-user key makes every raw
             value of theirs unreadable (test); source-mapped frames.
After:       O-1, X-1
```

```markdown
ID:          O-4
Goal:        The post-deploy browser check (design 9.6 level 1) as the stage check of a
             deployment.
May change:  crates/ruum-core/src/apps/check.rs (or a worker crate), the headless browser runner.
Done when:   a preview with a console error, a failing request or a 500 reports them in
             AppDeployment.check; new issues are linked.
After:       W-2, O-1
```

```markdown
ID:          O-5
Goal:        Logging helpers and the SDK for Node (`@ruum/log`, `@ruum/app`): message templates
             with named fields, the lint rule that blocks secret fields in log calls, the
             Ruum-User check.
May change:  sdk/node/ (new folder).
Done when:   the SDK verifies tokens against the app JWKS and refuses another app's token; the
             lint rule finds a secret field in a log call. Publishing to npm is a person's step.
After:       G-1
```

```markdown
ID:          X-1
Goal:        Issues: the provider ruum.issue, createIssue, listIssues, getIssue, updateIssue,
             listIssueEvents; template validation; regressions; mutes that expire.
May change:  crates/ruum-core/src/issues/, crates/ruum-api/src/routes/issues/.
Done when:   T-4's issue cases pass; a mute longer than 30 days is refused; a closed error
             issue reopens as regressed.
After:       C-3, L-1 (the gallery issue templates)
```

```markdown
ID:          X-2
Goal:        Reports, receipts and reveal requests: createIssueRevealRequest,
             resolveIssueReceipt, decideIssueReveal; the hold and its end.
May change:  crates/ruum-core/src/issues/{reports,reveal}.rs, routes/issues/receipts.rs.
Done when:   a refused field cannot be asked again; an allowed field is decrypted only into the
             issue; the hold ends at close or after 30 days; receipt tokens are stored as hashes.
After:       X-1, O-3
```

```markdown
ID:          L-1
Goal:        Templates: listTemplates, getTemplate, createTemplate, the gallery folder with the
             first templates (web app + Postgres + sign-in, static site, internal tool; bug,
             feature, question, error).
May change:  crates/ruum-core/src/templates/, routes/templates/, gallery/.
Done when:   createTemplate never copies full data, secrets, sessions or logs; gallery templates
             are readable by every signed-in principal and never a node of another tenant.
After:       C-3
```

```markdown
ID:          L-2
Goal:        instantiateTemplate and getTemplateSetup: the setup steps and role placeholders.
May change:  crates/ruum-core/src/templates/setup.rs.
Done when:   "web app + Postgres + sign-in" makes a database, an app with a ready preview and
             issue templates in one call; guests need guests_create_resources; an agent can do
             it within its scope (the agent benchmark's first task).
After:       L-1, W-2, D-1 (step 2)
```

```markdown
ID:          V-1
Goal:        Public projects: getProjectPublication, updateProjectPublication, the anonymous
             read paths (repo reads come with step 4; database: a public schema through
             queryDatabase for anonymous callers, an additive contract change).
May change:  crates/ruum-core/src/publication/, routes/projects.rs.
Done when:   only chosen items get the tuples; personal columns are hidden markers for the
             public; public queries are rate limited and cached.
After:       restricted folders (decision 8), W-1, the platform admin console (approval)
```

```markdown
ID:          M-2
Goal:        MCP tools and CLI commands of step 3 as their operations land.
May change:  crates/ruum-mcp/, crates/ruum-cli/.
Done when:   the agent benchmark task "deploy a static site and a Node app with Postgres from a
             template" passes over MCP against the laptop stack.
After:       W-2, W-3, O-2, X-1, L-2
```

```markdown
ID:          U-3
Goal:        The UX package of step 3 (design 18.4.1): journeys (deploy a preview, publish,
             roll back, give an app role, read an error, file and follow a report, start from a
             template), wireframes, the copy of every new state, contract needs.
May change:  docs/ux/apps/ (new).
Done when:   the UX review panel accepts; contract needs are filed for C-3 before it freezes.
After:       none
```

```markdown
ID:          U-4
Goal:        The web UI of step 3 (see the area).
May change:  web/.
Done when:   the UX review panel accepts the screens; axe and visual snapshots pass.
After:       U-3 and the operations each screen needs
```

```markdown
ID:          T-5
Goal:        Harden step 3: the sandbox and telemetry suites strict, the mutation check, two
             security reviewers (sandbox and privacy lenses), load test of the gateway, the
             agent benchmark with app tasks, synthetic users on the app pages.
After:       the build cards
```

## Order

1. **Specify** (now): C-3 review; S-2 and U-3 in parallel; T-4 after C-3.
2. **Foundations** in parallel: R-1, O-1 (after I-5), I-4 (after S-2), I-5, W-1, L-1.
3. **Runtime and gateway**: R-2, G-1, B-1; then W-2 (needs step 2's D-3 for masked branches;
   until D-3 lands, a preview with a database binding fails at stage `bindings` with
   `conflict`, hint `database_branches_not_ready`), W-4, B-2, I-6.
4. **Publish**: W-3 (needs step 2's D-2), O-2, O-4, X-1, R-3.
5. **Reports and more**: O-3, X-2, G-2, G-4, L-2, W-5, B-3, G-3, O-5, R-4.
6. **Clients**: M-2 and U-4 as their operations land; I-7, I-8.
7. **Harden**: T-5.
8. **Later**: V-1 after restricted folders.

Static apps can ship first: W-1, W-2 with `runtime: static` (no image, no sandbox: files from
Garage through the gateway), G-1, B-2 give a usable step before R-2 lands.

Migrations go through the queue: 0011 is the only migration of the step; a change during step 3
is made in place by a migration task (it is not released), as in step 2.

## Open questions for the panel

The full list with options and recommendations is `ruum-server-wt/panel/step3-open.md` (its
numbers differ; the numbers below are the ones this page and decision 0006 use). In short:

1. The gateway as its own binary (`ruum-gateway`) or a module of the server? Recommended: its own
   binary (another domain, scales and fails alone).
2. App principal per environment (live = node ID, preview = deployment ID)? Recommended: yes.
3. The builder for `node22`: Railpack, Nixpacks or Cloud Native Buildpacks? Recommended: Railpack.
4. The telemetry pipeline: OTel Collector processors from a generated config, or a small Rust
   processor? Recommended: a Rust processor for classes and pseudonyms, the Collector for the rest.
5. The egress proxy: Envoy or Squid? Recommended: Envoy.
6. Who may file an issue: `edit` or `comment` on the folder? Recommended: `comment`.
7. Agent publishes always wait for a person in step 3 (before project.yaml approvals)?
   Recommended: yes.
8. Agents and secret values: never in step 3? Recommended: yes.
9. Rollback across migrations: forward-only with `force`? Recommended: yes.
10. Issues as typed nodes, not files? Recommended: typed.
11. Raw-copy grants outside reports? Recommended: not in step 3.
12. Name recognition in free-text scrubbing: which model, where? Recommended: patterns only in
    step 3; free text without a template is `personal` anyway.
13. Reserved tenant slugs (`content`, `www`, `api`, `admin`, `static`, `internal`, `ruum`)?
    Recommended: reserve these.
14. The platform admin console API: a separate contract (`contracts/admin/`)? Recommended: yes,
    with its own card before public apps go live.
