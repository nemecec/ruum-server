# 0006: Step 3 specification decisions (draft for the panel)

Status: `provisional`, DRAFT (design section 18.9). Proposed by the step 3 spec run on branch
`s3/spec` (2026-11-10); the architect panel has not reviewed them yet (card C-3 in
`docs/STEP3.md`). A person can veto any item. The open questions are in
`ruum-server-wt/panel/step3-open.md`.

## 1. The app gateway is a binary of its own

`crates/ruum-gateway` serves the apps domain (`*.ruumapps.net`), not the core server. Reasons: it
answers another registered domain (a bug in it must not touch the platform origin), it carries
all app traffic (it scales and fails alone, design 12: "split a module when it needs to scale on
its own"), and it holds the WebSockets. It reads the core through the authz engine (SpiceDB), a
host cache fed by the journal, and a small internal API of the core. The internal gateway for
app-to-app calls is the same binary on the app networks.

## 2. A version of a web app is one live deployment

Design 9.1: "Each version is one deployment. Rollback is restore version 12." A version is
written at **publish** (and at rollback), not for previews, so the release history is
`listVersions` and `restoreVersion` is a rollback. The encoding is `app_release_v1`: the
canonical JSON of the deployment ID, the archive's chunk list and the parsed `app.yaml`, so the
blob GC sees every chunk of every release. Previews are rows of `app_deployments` whose chunk
list keeps their archive alive until the preview expires. `putContent` and uploads keep refusing
typed nodes (decision 0004-6); an archive enters through `createAppDeployment` from a file node.

## 3. No privilege escalation through bindings

Design 9.4. At deploy time, for every binding the deployer must hold the binding's role on the
target (for an agent: its user's right intersected with its scope), and for an API binding the
right to grant its scopes (`app_api_scope#grant`: `manage` on the called app, or the app role
named in `granted_by`). Else the stage `bindings` fails (`403 permission_denied`, hint
`binding_not_allowed`). `app_bindings.granted_*` records the deployer. An app never gets `manage`
on a node (`app.yaml` allows `view` and `edit`); migrations run as the platform at deploy time with
the deployer's right (`manage` on the database for live data). At publish the check runs again
for the live roles.

## 4. An agent's publish waits for a person

Design 11.1 ("Publish needs the usual approval") and decision 39 ("publish, with the same
approvals as in a session"). Until step 5 brings `project.yaml` approval rules, every publish or
rollback by an agent answers `202` with an `AppApproval`; the agent's user or a person with
`manage` on the app decides (`decideAppApproval`, users only), with every right the publish needs;
the agent then sends the same call with `approval_id` (used once, within 15 minutes, for exactly
that deployment). Previews need no approval. An agent cannot roll back through `restoreVersion`
(`403`, hint `publishAppDeployment`).

## 5. Previews never touch live data

Decision 30 and design 8.5. A preview is a principal of its own (the deployment ID; the live
principal is the app node ID). Its database bindings resolve to a branch with `masked` data
(shared by previews with the same `draft` label); its other node bindings are capped at `view`;
its API bindings call the live API of the other app with the binding's scopes; generated secrets
get preview values; entered secrets reach it only with `preview: true`. A preview is reachable
only by people with `edit` on the app and expires after 7 days without a request.

## 6. Issues are typed nodes

Design 9.7 says "a file of type `issue`". The draft makes an issue a typed node (provider
`ruum.issue`, content `contracts/manifests/issue.schema.json` as versions), so its content is
validated against its template like a manifest, counts live in provider state without making a
version per occurrence, and `putContent` cannot write an unvalidated issue. Everything else of
9.7 holds (versions are history, node comments, tags as labels, boards as smart folders). Open
question 10 asks the panel to confirm; a design text change follows the decision.

## 7. Agents never handle secret values in step 3

Design 9.9 allows "an agent with that person's approval" to paste an entered secret. Carrying a
secret through an approval would put the value in an approval record or an agent transcript.
In step 3 `setAppSecret` is users only; agents see names and states (`listAppSecrets`) and tell
their person what is missing. Generated secrets need nobody.

## 8. The pseudonym of a known value is for people

`computeAppPseudonym` (design 9.6, "what happened to kati@example.com?") is users only and rate
limited: an agent could otherwise test guesses against the logs (the same oracle concern as
decision 0004-24).

## 9. No new error codes

Every new refusal uses an existing code with a hint: `permission_denied` (`binding_not_allowed`,
`public_apps_not_allowed`, `custom_domains_not_allowed`, `tenant_admin_required`,
`publishAppDeployment`), `conflict` (`secret_missing`, `raw_copy_gone`,
`database_branches_not_ready`), `user_required`. So `ErrorCode`, its Rust list and the web copy do
not change with the contract. A card may still propose a code in a contract change if a hint is
not enough for clients.

## 10. Three credentials that never mix

The REST API accepts only API tokens (`aud` = the API origin). The runtime API at `ruum.internal`
accepts only workload tokens (`typ: app_workload`, `aud: ruum.internal`, mounted by the runtime).
Apps verify only user tokens (`typ: app_user`, `aud: app:<id>`, from a key set apart from the API
keys). A replay across any two fails by `aud` and `typ`.

## 11. The gallery is part of the platform

Gallery templates (`gallery:<slug>`) ship with the platform release, not as nodes of a system
tenant, so `instantiateTemplate` never reads another tenant (the step 1 rule "no operation reads
or writes two tenants" holds).

## 12. Approvals of the platform admin are tenant state

`Tenant.app_approval` (read-only through the API) and `tenant_app_settings` hold what a Ruum
platform admin approved (public apps, custom domains; decision 22). The admin console that sets
them has its own contract later (open question 14). The suite fixtures have both approvals, so
the permission rules test the positive case.

## 13. Telemetry follows the classes of 8.5 at ingestion

`secret` is never stored, `personal` is a keyed pseudonym with the tenant key (the step 2
`Pseudonymizer`, so a person can find a known value), unclassified is `personal`, and personal
metric labels are dropped. The raw copy is off by default (`raw_copy_days: 0`) and is decrypted in
step 3 only into an issue by the reporter's choice.

## 14. Public projects wait

`getProjectPublication` and `updateProjectPublication` are in the contract (planned) so the shape
is reviewed with the rest, and card V-1 builds them after restricted folders (design 16).
