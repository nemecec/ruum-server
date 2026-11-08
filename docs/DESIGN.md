# Ruum: files, photos, databases and apps in one tree

Status: draft 7, 2026-11-06. Product: **Ruum** (`ruum.space`). "The platform" in this text means Ruum.

This page proposes a design for a multi-tenant cloud service. It stores files and photos like
Google Drive or Dropbox. It also hosts databases and web applications, and it lets other
programs (first Kadakas CAD) keep their documents in the same tree. AI agents are a main user:
they build applications on the platform.

## 1. The one idea

**Everything is a node in a tree, and one permission system protects all nodes.**

A file, a folder, a photo album, a Postgres database, a web application, a source repository and
a Kadakas drawing are all nodes. Each node has a parent, a name, versions, tags and grants. The
core of the platform knows only four kinds of node: folder, file, link and *typed node*. A typed
node is a "placeholder": the real content lives in a **provider** (a plugin). Databases, web apps
and repositories are first-party providers. Kadakas CAD is the first external provider. All of
them use the same provider contract (section 7).

This gives four results:

1. Sharing works the same way for everything. "Share the folder `Garden planner` with Mari" gives
   Mari the app, its database and its photos in one step.
2. Versioning works the same way for everything. A new deployment of a web app is a new version
   of the app node. A rollback is "restore an old version".
3. AI agents learn one model. "Create a node of type `database` with this manifest" makes a
   database. "Upload a new version of the `webapp` node" deploys.
4. The plugin mechanism is tested every day, because the first-party features use it too.

## 2. The node model

### 2.1 Kinds of node

| Kind | Content | Examples |
|---|---|---|
| `folder` | children | folders, albums, projects, encrypted vaults (section 5.5) |
| `file` | a list of versions, each a blob | documents, photos, deployment archives |
| `link` | a target node ID | shortcuts, album entries |
| `typed` | a manifest (small JSON/YAML, versioned) + provider state | database, webapp, repo, Kadakas drawing |

A folder has an optional `type` too. An album is a folder of type `album`. A project is a folder
of type `project`. The type changes the user interface, not the core rules.

### 2.2 Identity and names

- Every node has a UUIDv7 ID. The ID never changes. Moves and renames change only `parent_id` and
  `name`.
- A name is unique among the live (not trashed) children of a folder.
- Links point to the **ID**, not to the path. A link stays correct when the target moves.
- Paths are only for display and for the API (`GET /v1/nodes/by-path/Projects/Garden`).

### 2.3 Links

There are two kinds of link. The difference is the permission rule:

| Link kind | Gives access to the target? | Use |
|---|---|---|
| `shortcut` | No. You see the target only if you already have access. | "Add to my drive", quick access |
| `member` | Yes. A person with access to the folder that holds the link can see the target with the same or a lower role (never higher than `view` by default). | albums, curated collections, "files used by this app" |

To make a `member` link, you need the `share` permission on the target. So nobody can leak a file
that they cannot share.

**Albums are folders of `member` links.** The
photo stays in its original folder (one copy, one version history). An album can hold photos from
many folders. Sharing the album shares those photos for viewing. Removing the link from the album
removes that access. Deleting the photo leaves a broken link, which the UI hides.

### 2.4 Tags and properties

- **Tags** belong to the tenant: `tags(id, tenant_id, name, color)` and `node_tags(node_id, tag_id)`.
  A user sees the tags of the nodes that the user can see. Personal labels ("favourite") are a
  separate per-user table.
- **Properties** are typed metadata in namespaces: `photo.exif`, `photo.place`, `kadakas.project`.
  Stored as JSONB with a few promoted columns (taken time, GPS point with PostGIS).
- A **smart folder** (a saved search, for example "tag = garden and taken in 2026") is a typed node
  whose children are computed.

### 2.5 The change journal

Every change to a node writes one row to the journal in the same database transaction:
`journal(tenant_id, seq, node_id, op, actor, on_behalf_of, at)`. `seq` grows per tenant. The journal feeds:

- the delta API for sync clients ("give me all changes after cursor 18233"),
- the event bus (thumbnails, search index, provider webhooks),
- the audit log.

Build this from day one. A sync client without it is very hard to add later.

### 2.6 Core tables (sketch)

```sql
create table nodes (
  id            uuid primary key,          -- UUIDv7
  tenant_id     uuid not null,
  parent_id     uuid references nodes(id),
  name          text not null,
  kind          node_kind not null,        -- folder | file | link | typed
  type          text,                      -- MIME type, 'album', 'database', 'kadakas.drawing', ...
  link_kind     link_kind,                 -- shortcut | member
  link_target   uuid,
  head_version  uuid,
  owner_id      uuid not null,
  size_bytes    bigint not null default 0,
  trashed_at    timestamptz,
  created_at    timestamptz not null,
  updated_at    timestamptz not null
);
create unique index on nodes (tenant_id, parent_id, name) where trashed_at is null;

create table versions (
  id            uuid primary key,
  tenant_id     uuid not null,
  node_id       uuid not null references nodes(id),
  manifest      bytea not null,            -- list of chunk hashes, or the typed-node manifest
  size_bytes    bigint not null,
  content_hash  bytea not null,            -- BLAKE3 of the whole content
  actor         uuid not null,             -- user, app or agent principal that did the write
  on_behalf_of  uuid,                      -- the user, when the actor is an agent or an app
  label         text,                      -- "Deployed by agent X", "Before import"
  created_at    timestamptz not null
);
```

Every table has `tenant_id`, and every query filters on it. Postgres row-level security on
`tenant_id` is a second guard. No query joins two tenants. This rule makes sharding (section 14)
possible later.

### 2.7 Templates

A **template** is a folder of type `template` that serves as a blueprint. "New … from template"
copies it and runs its setup. Two kinds in the first versions:

- **Project templates**: the folder structure, `project.yaml` (agent settings, bindings), an
  `AGENTS.md`, issue templates, database manifests with migrations and masked sample data, an app
  scaffold in a repo, smart folders, and placeholders for roles ("client: comment"). The setup
  creates the database, the repo, the app and the first preview, then asks for the role
  placeholders. Examples: "web app + Postgres + sign-in", "client portal", "internal tool",
  "static site", "landscape project" (with Kadakas drawings).
- **Issue templates** (section 9.7): the fields of one kind of issue.

Templates come from Ruum (a gallery), from the tenant (any project can be saved as a template), and
later from other people (a public gallery). A template has versions, and a project records the
template and version it came from, so it can later be **updated from the template** (a diff and
merge through the plugins of section 5.4, reviewed like any change). Agents use templates through
the API and MCP ("create a project from a template").

## 3. Permissions

### 3.1 Principals

| Principal | Who |
|---|---|
| `user` | a person; can have many linked identities (section 4) |
| `group` | a set of users or groups, managed in the platform or synced by SCIM |
| `app` | a deployed web app (its own identity) |
| `agent` | an AI agent with **its own identity and credentials**; always acts **for** a user, with a narrower scope (section 3.4) |
| `tenant#member` | "everybody in the organisation" |
| `anyone_with_link` | a secret share link (optional password, expiry) |
| `public` | everybody, also without sign-in (for public web apps and published files) |

### 3.2 Roles and how they map to typed nodes

One small set of roles applies to all nodes. Each node type says what a role means for it:

| Role | File / folder | Database | Web app | Repo | Kadakas drawing |
|---|---|---|---|---|---|
| `view` | read, download | read-only queries | use the app (plus app-defined roles, section 9.3) | read code | open read-only |
| `comment` | comment | - | - | comment | comment |
| `edit` | change, upload versions | read-write queries | deploy a new version | push, merge | edit |
| `manage` | move, delete, restore | schema, size, backups, branches | domains, secrets, bindings | settings, CI | settings |
| `share` | change grants | change grants | change who may use it | change grants | change grants |

Since decision 0001-1 (`docs/decisions/`), `share` is a permission, not a role: `manage` always
has it, and `edit` has it while the node setting `editors_can_share` is true (as in Google Drive).

In a team tenant, a member gets a *content role* on the whole team tree (decision 0003-1, the
roles of a Google shared drive): `view`, `comment`, `edit` or `manage` (the default). Owners and
admins always manage the team tree; guests have only grants. A "Can view" member still sees
everything in the team tree and changes nothing; to show a person only some folders, make them
a guest. Grants still only add rights: an `edit` grant on a folder lets a `view` member edit
there, and a grant to everyone in the tenant reaches every member.

Roles are inherited from parent folders. A grant on a child can add rights, but it cannot remove an
inherited right (this is the Google Drive rule; it is simple to explain). A folder can be marked
"restricted" to stop inheritance from above, for confidential sub-folders.

### 3.3 The engine: relationship tuples (Zanzibar model)

Inheritance, groups in groups, `member` links and apps as principals are a graph problem. Use a
Zanzibar-style engine. Recommendation: **SpiceDB** (open source, Postgres storage, consistency
tokens). OpenFGA is a good alternative. Schema sketch:

```zed
definition user {}
definition group   { relation member: user | group#member }
definition tenant  { relation admin: user  relation member: user | group#member }
definition app     { relation tenant: tenant }
definition agent   { relation acts_for: user }

definition node {
  relation tenant:     tenant
  relation parent:     node
  relation collection: node                  // folders that hold a `member` link to this node
  relation owner:      user
  relation manager:    user | group#member
  relation editor:     user | group#member | app | tenant#member
  relation viewer:     user | group#member | app | tenant#member | user:*

  permission manage = owner + manager + parent->manage
  permission edit   = manage + editor + parent->edit
  permission view   = edit + viewer + parent->view + collection->view
  permission share  = manage
}

// Roles that a web app declares in its manifest (section 9.3). ID: "<app-node-id>/<role>".
definition app_role {
  relation app:    node
  relation holder: user | group#member | tenant#member
  relation granter: app_role                  // the app role that may grant this one, if any
  permission has      = holder
  permission grant    = app->share + granter->has
}
```

Two practical points:

- **"Shared with me" and search need lists, not checks.** Asking the engine "which of 2 million
  nodes can Mari see?" is slow. Keep a denormalised table `share_index(principal, node_id)` for
  the roots of grants, and filter search results with batch checks.
- **Agents and apps get the intersection.** An agent token says: acts for user U, may touch only
  subtree S, only roles R, until time T. The effective right is `U's right ∩ scope`. The same
  rule applies to an app that acts "on behalf of" the signed-in user.

### 3.4 Agent identity: who did it, and for whom

An agent is a principal of its own, not a copy of the user's session. So the history can always
say "Agent *Garden builder* changed this for Mari", never only "Mari changed this".

- **Registration**: a user registers an agent ("Claude in my laptop", "Nightly report bot"). The
  agent belongs to that user. Tenant admins see all agents in the tenant and can disable them.
- **Credentials**: the agent gets its own credentials, never the user's password or session:
  - an OAuth client (preferred; MCP clients use dynamic client registration and the user
    approves the scope in a consent screen), or
  - an **API key** for scripts. Keys have a visible prefix (`ruum_agent_...`) so secret scanners
    find leaks, are stored only as a hash, have an expiry, and are exchanged for short-lived
    access tokens. A key carries the same scope as above (subtree, roles, budget).
- **Every write records two principals**: `actor` (the agent) and `on_behalf_of` (the user). The
  journal, versions, comments, audit log and the database proxy (as `application_name` and in an
  audit table) all store both. The UI shows a small agent badge on versions and changes.
- **Rights**: the agent's right is the user's right ∩ the agent's scope. If the user loses access,
  the agent loses it too. If the user leaves the tenant, the agent is disabled.
- **Filters**: "show only changes by agents", "undo everything agent X did since 14:00" (possible
  because every change is a version with an actor).
- **Team agents** (later): an agent owned by a group instead of a person, for shared automations.
  It acts for the group, and a group admin is responsible for it.

The same `actor` / `on_behalf_of` pair is used for apps that act on behalf of a user.

## 4. Tenants and identity

### 4.1 Tenants

- A **tenant** is an organisation that pays. A private person is a tenant with one member.
- All data of a tenant lives in one **cell** (section 14) and one region. This gives data
  residency ("all data stays in Finland/Germany"), which is a selling point in the EU.
- A user can be a member of many tenants (a consultant). People from other tenants get access as
  **guests** of the owner tenant. Shared data never moves to the guest's tenant.
- Per tenant: quotas, retention rules, allowed sharing (for example "no public links"), allowed
  identity providers, audit log, billing.

### 4.2 Identity and SSO

Do not write the identity provider yourself. Recommendation: **Zitadel** (open source, built for
multi-tenant use with "organisations", OIDC and SAML, passkeys, SCIM, Postgres). Keycloak is the
alternative, but thousands of realms are hard to run; its newer "organizations" feature helps.

- The platform has its own OIDC issuer. All services trust only platform tokens (asymmetric
  keys, published as JWKS). Third-party SSO is upstream of this issuer.
- A user has many **identities**: Google, Microsoft, Apple, the SAML or OIDC provider of the
  employer, a passkey. Link identities only by an explicit action of the signed-in user, or by a
  verified email from a trusted provider. Automatic linking by email alone allows account takeover.
- **Enterprise SSO per tenant**: the tenant admin adds their IdP (Entra ID, Okta, Google Workspace,
  any SAML/OIDC), verifies their email domain by a DNS record, and can require SSO for that domain.
  SCIM keeps users and groups in sync and removes leavers.
- **Machine identities**: OAuth 2.1 clients for apps and agents (with dynamic client registration
  for MCP clients), personal access tokens with scopes, and token exchange (RFC 8693) when an app
  calls the platform on behalf of a user.

### 4.3 Plans and limits

A first proposal. The numbers are configuration, not code, so they can change at any time.

| | Free | Personal | Team (per member) |
|---|---|---|---|
| Storage | 5 GB | 200 GB | 1 TB, shared by the team |
| Largest file | 2 GB | 50 GB | 50 GB |
| Version history | 30 days | 180 days or 100 versions | 1 year or 500 versions; named versions forever |
| Trash | 30 days | 30 days | 30 days (admins can restore for 60) |
| Databases | 1 SQLite + 1 Postgres, 250 MB each | 10, 5 GB each | 25 per member, 10 GB each |
| Web apps | 3, private only, scale to zero | 10, scale to zero | 25 per member, "always on" as an option |
| Agents | 2 | 10 | 10 per member |
| Public apps, custom domains | no | only after approval by a Ruum admin | only after approval by a Ruum admin |

Each agent and project also has a monthly compute and egress budget with a hard stop (section 11).

## 5. File storage, versions and backups

### 5.1 Blobs

- Files are cut into chunks with content-defined chunking (FastCDC, about 1-4 MiB). Each chunk is
  addressed by its BLAKE3 hash and stored once **per tenant** in object storage
  (`/<tenant>/<hash-prefix>/<hash>`). No dedup across tenants: it leaks information and makes
  per-tenant encryption and deletion hard.
- A version is a manifest: the ordered list of chunk hashes. A small edit to a large file adds only
  the changed chunks. So versioning costs little.
- Uploads are resumable (tus protocol or S3-style multipart). The client sends the hashes first and
  uploads only the chunks that the server does not have.
- Encryption at rest: each tenant has a data key, wrapped by a key in OpenBao (the open-source
  Vault fork). A tenant can be "crypto-shredded" by deleting its key.
- **Customer-managed keys** (later, for large customers): the tenant key is wrapped by a key in the
  customer's own KMS. The customer can cut off access by revoking it. All server features keep
  working. This is often what an organisation means when it asks for "end-to-end encryption";
  real end-to-end encryption is section 5.5.

### 5.2 Three layers that people often mix

| Layer | For | Who restores | Example rule |
|---|---|---|---|
| **Versions** | "I want yesterday's file" | the user | keep 100 versions or 180 days, plan-dependent; named versions forever |
| **Trash** | "I deleted it by mistake" | the user | 30 days, then purge |
| **Backups** | "the data centre burned" or "a bug deleted rows" | operators | Postgres point-in-time recovery; object storage replicated to a second site and a second provider |

Backups follow 3-2-1: the metadata database ships WAL to object storage (pgBackRest or the
CloudNativePG backup), and the blob store is copied to a second location (another Hetzner site) and
to a second provider with write-only, immutable credentials (object lock where the provider
supports it). Ransomware in one account must not be able to delete the backup. Test a restore
every month; an untested backup is not a backup.

Garbage collection of chunks is mark-and-sweep over all manifests of the tenant, with a grace
period, so an upload in progress is never deleted.

### 5.3 Pluggable versioning

Make versioning pluggable, but split it into three separate parts. Only the third one is visible
to the user as "Git-like".

**Part 1: storage encoding (invisible).** Keep the snapshot model: every version is a complete
manifest, and the API always returns a whole file. Under it, the encoding can differ by type:

| Content | Encoding |
|---|---|
| default | content-defined chunks of 1-4 MiB, zstd where it helps |
| text, source, JSON, CSV, small files | smaller chunks (16-64 KiB), or a zstd delta against the previous version (`--patch-from`); a full snapshot every N versions so that a restore never reads a long chain |
| already compressed (JPEG, video, ZIP) | large chunks, no compression |

Git itself works the same way: it stores full snapshots and makes deltas only inside pack files.
Storing "diffs" as the primary format makes restore slow and a broken link in the chain loses all
later versions. Snapshots with deltas underneath give the savings without that risk. For text the
saving is small anyway: a 20 KB file with 500 versions is 10 MB before compression.

**Part 2: version policy (per tenant, folder or type).**

- *Coalescing*: an editor that saves every 5 seconds does not make 700 versions an hour. Saves by
  the same actor within a window (for example 10 minutes) merge into one version. Named versions
  never merge.
- *Thinning*: keep all versions for a day, then one per hour for a week, then one per day, up to the
  plan limit (like Time Machine). Named versions are kept.
- *Retention and legal hold* from the tenant policy.

**Part 3: version semantics (plugins by type).** This is where the real value of "Git-like" is.
Compare, merge and line history are done by **diff and merge plugins**, chosen by file type
(section 5.4).

**Typed nodes bring their own versioning.** The version API (list, get, diff, restore, label) is the
same for all nodes, but the provider implements it: a repo maps versions to commits, a database to
backup points and branches, a Kadakas drawing to its snapshots, a web app to deployments. So the
user sees one "Version history" panel everywhere.

When a person wants real branches, merges and history across many text files, the folder should be
a `repo` node (section 10), not a folder of files with Git-like versions. A repo can be created from
a folder ("turn this folder into a project with history").

### 5.4 Diff and merge plugins

A line diff is good for prose and bad for almost everything else: reformatted code looks like
everything changed, an XML file on one line is one changed line, and a DOCX file is a ZIP file. So
compare and merge are plugins, and a plugin can use other plugins.

**A plugin is a pipeline of three kinds of step:**

| Step | What it does | Examples |
|---|---|---|
| **unpack** | opens a container into named parts | ZIP (DOCX, XLSX, ODT, IFC-ZIP, app archives), tar, email (MIME) |
| **normalise** | makes equal content look equal | XML canonical form (C14N) and pretty-printing, JSON with sorted keys, line endings, code formatting |
| **compare** | finds the changes in one model | lines and words (text), syntax tree (code), element tree (XML), document model (paragraphs, runs, tables, styles), pixels (images), objects (CAD) |

Examples:

- **Source code**: parse both versions with **tree-sitter** (grammars for about 200 languages) and
  compare the syntax trees. A moved function is a move, not a delete and an add, and a change of
  indentation is not a change. Prior art in Rust: *difftastic* (structural diff) and *mergiraf*
  (syntax-aware three-way merge). Both can be used as libraries or as models.
- **DOCX**: unpack the ZIP → take `word/document.xml`, the styles and the comments → build a
  document model (paragraphs, runs, tables, images) → compare paragraphs, then words inside changed
  paragraphs. Formatting changes ("made bold") are a separate kind of change, so they can be hidden.
  The result is shown as a redline, like Word's "Compare". It can also be **exported as a DOCX with
  tracked changes**, so people can accept or reject changes in Word. A first version can be much
  simpler: convert both versions to Markdown with pandoc and diff the text.
- **Any XML**: canonical form, then an element-tree diff that knows about attributes and the order of
  children. Plugins for XML dialects (SVG, BPMN, XLIFF) add knowledge of what an element means.
- **Spreadsheets** (XLSX, CSV): changed cells, inserted rows and columns, changed formulas.
- **Images**: side by side, swipe, and a pixel-difference overlay. Changed EXIF is shown as metadata.
- **Kadakas drawings**: the provider compares objects (added, removed, moved, changed properties)
  and highlights them on the drawing.

**Choosing the plugin.** Each plugin declares what it matches (MIME types, extensions, a content
check) and a priority. The most specific plugin wins, with a fallback chain: special plugin →
structural (XML/JSON/syntax tree) → text → binary ("the files differ: 2.1 MB → 2.3 MB"). When more
than one plugin matches (a `.rs` file: syntax tree or plain text), the user can switch the view.

**One result format.** All plugins return the same change model, so one UI shows all diffs and the
API gives agents structured changes instead of text to parse:

```json
{
  "plugin": "docx@1.4.0",
  "summary": { "inserted": 3, "deleted": 1, "modified": 5, "moved": 1 },
  "changes": [
    { "kind": "modify", "path": "body/p[14]", "label": "Section 4.2, paragraph 3",
      "old": "Payment within 30 days.", "new": "Payment within 14 days.",
      "detail": [{ "kind": "replace", "old": "30", "new": "14" }] },
    { "kind": "format", "path": "body/p[2]/r[1]", "label": "Title", "change": "bold on" }
  ],
  "render": { "html": "/v1/diffs/9f2.../render.html" }
}
```

The `path` is stable inside a format, so comments ("why 14 days?") can point at a change, and a
model can write the plain-language summary of a version from the changes, not from raw text.

**Merge uses the same models.** A plugin with a `compare` step can offer `merge(base, ours, theirs)`:
two changes in different paragraphs, functions or cells merge without conflict. When two sync
clients change the same file offline, the platform tries the merge plugin first. If it finds a real
conflict, it makes a conflicted copy, as before.

**How plugins run.** Plugins read user content, and some will come from third parties. So a plugin
is a **WebAssembly component** (WASI, run with Wasmtime) with limits on time and memory and no
network access. A plugin that needs more (a provider with its own data) is called through the
provider contract instead. The same Wasm module runs:

- on the server, for the web UI and the API;
- in the desktop, mobile and web clients, which is the only place where diffs of **vault** files
  (section 5.5) can run, because only the client has the keys.

Results are cached by `(plugin version, hash of a, hash of b)`. Diffs are computed when someone
asks, not for every new version. Exceptions are small text files and the version summaries.

### 5.5 End-to-end encrypted vaults

End-to-end encryption is available per folder. A **vault** is a folder of type `vault`. Inside a
vault the server stores only ciphertext, and only the members' clients hold the keys. Nodes
outside vaults are not affected.

**What still works in a vault, and what does not:**

| Works | Does not work (or only on the client) |
|---|---|
| upload, download, folders, moves, trash | server thumbnails, previews and the photo pipeline (the client makes encrypted thumbnails itself) |
| versions and restore (each version is ciphertext) | full-text and content search on the server (only local search in the client) |
| sharing with named platform users | public links and "anyone with the link" (an optional exception: the key in the URL fragment, which the server never sees) |
| sync, quotas, backups of the ciphertext | typed nodes: databases, web apps, providers, mounts cannot live in a vault |
| tags (encrypted) | server-side agents; only an agent that runs on the user's own device with the keys can work in a vault |
| | diff and merge plugins on the server (they run in the client) |
| | recovery by the platform: if all keys are lost, the data is lost |

**What the server can still see** (be honest about this in the product): that the vault exists,
its members, the number of items, the tree shape, sizes (rounded up to buckets), times and who made
each change. File and folder names are encrypted. To keep names unique without knowing them, the
server stores a keyed hash of each name (`HMAC(vault_key, name)`).

**Key model** (use a proven design and audited libraries; do not invent crypto):

- Each user has an identity key pair (X25519 for encryption, Ed25519 for signatures) and each device
  has its own keys. The private key is protected by a key from a **passkey (WebAuthn PRF
  extension)** or a passphrase (Argon2id), plus a printed **recovery key**.
- Each vault has a vault key with **epochs**. The vault key is wrapped for each member's public key.
  Each file has its own file key, wrapped by the vault key. Chunks are encrypted with
  XChaCha20-Poly1305 or AES-256-GCM.
- **Chunks and dedup**: chunk IDs are `HMAC(file_key, plaintext_chunk)`, so a new version uploads
  only changed chunks of the same file. There is no dedup between files or vaults; that would leak
  information.
- **Adding a member** wraps the current vault key for them. **Removing a member** starts a new
  epoch: new content uses the new key. Old content stays readable by the removed member only if they
  had downloaded it already, which no system can prevent.
- **Public keys must be verifiable**, or the server could give a false key and read everything. Show
  a safety code that members can compare, and later publish keys in a key-transparency log.
- For larger group vaults, a group key protocol such as MLS (RFC 9420) handles many members and key
  changes better than wrapping keys one by one.

**Organisations** often need recovery ("the employee left and the vault is locked"). Offer an
optional **organisation recovery key**: the vault key is also wrapped for a key that the tenant
admins hold, offline. Only those admins hold this key; members never see or use it. What members
see is **that the vault is recoverable**: the vault shows a label such as "Acme can recover this
vault" (or "Only members can recover this vault"), also before anyone adds files. The vault is
still end-to-end encrypted against the platform, but not against the organisation, and members
must know that.

**The web client is the weak point.** The server delivers the JavaScript that holds the keys, so a
hacked or forced server could deliver code that steals them. The desktop and mobile apps (signed,
updated through the stores) are much stronger. Options: vaults only in the native apps at first,
or a web client with a strict CSP, Subresource Integrity and published build hashes, with this risk
written in the documentation.

## 6. Photos

A photo is a file. The photo feature adds a pipeline and a different view:

1. The upload writes the version. The journal event goes to the media workers.
2. Workers read EXIF/XMP (time, GPS, camera, orientation), make renditions (thumbnail, preview,
   web size) with libvips, handle HEIC and RAW, and transcode video with ffmpeg.
3. Renditions are attached to the **version hash**, not stored as versions. Two equal files share
   renditions.
4. Optional, per tenant: reverse geocoding (place names), image embeddings for "search by
   content" (pgvector), and face grouping. Face data is biometric data under GDPR (special
   category), so it must be opt-in, per tenant and per user.

The photo view is a query over properties ("all nodes of type image/* that I can see, ordered by
taken time"), not a folder. Albums are folders of `member` links (section 2.3). Smart albums are
saved searches. Location strip: a per-album or per-share option "remove GPS when shared publicly".

## 7. Typed nodes and providers (the plugin mechanism)

### 7.1 The contract

A provider is a registered application with:

- **Node types** that it owns, for example `kadakas.drawing`, with icon, name and file extension.
- **Open**: the platform sends the user to `https://provider/open?node=<id>` with a short-lived
  signed token (`aud` = provider, `sub` = user, `node`, `roles`, `exp`). This is the same idea as
  Microsoft's WOPI protocol and the Google Drive "open with" flow.
- **Check API**: the provider can ask the platform at any time: "may user U edit node N?". It also
  subscribes to permission events, so it can close an open editing session at once when access is
  revoked.
- **Lifecycle hooks** (webhooks with retries, from the journal): created, **copied** (the provider
  must copy its content; this case is often forgotten), moved, renamed, trashed, restored, purged,
  tenant exported, tenant deleted.
- **Optional features**: thumbnail and preview, export formats (download a drawing as DXF or IFC),
  text for full-text search, version list (the provider reports snapshots so they appear in the
  version history; restore calls the provider).

### 7.2 Where the content lives

Two options. Prefer the first:

1. **Content in platform storage.** The provider writes its snapshots as versions of the node
   through the blob API. Then backups, versions, quotas, export and deletion work without extra
   work. The provider keeps only hot state (for example the live editing session).
2. **Content in the provider.** The provider stores the data itself and must implement export and
   purge. Use this only for external systems (GitHub).

### 7.3 Kadakas CAD as the first provider

The Kadakas collaboration server (`kadakas-server`) already has documents, a log and snapshots in
Postgres. To become a provider it needs:

- Accept platform tokens (RS256/EdDSA, checked with the JWKS) instead of the shared HS256 secret.
  `auth.rs` is already a trait, so this is a new implementation.
- Map `documents.id` to a node ID, and take the role (view/comment/edit) from the token.
- Listen to permission events and disconnect clients that lost access.
- Write snapshots as node versions (option 1 above), so the drawing has the same history UI as a
  file.

This integration comes after databases, web apps and repos (section 16). Those first-party
providers define the contract. Kadakas then tests the parts that they do not use much: the "open"
flow with signed tokens, live revocation of editing sessions, and copy and export hooks.

### 7.4 Mounted external drives (later)

A user can mount an external storage account into the tree: Google Drive, OneDrive or SharePoint,
Dropbox, an S3 bucket, WebDAV or an SMB share. A mount is a typed node of type `mount` that acts
as a folder. Its children come from the external system. Each external system is a **mount
provider**: the provider contract of 7.1 plus a listing API (`list`, `stat`, `read`, `write`,
`changes since cursor`).

```yaml
# /Mari/Google Drive   (type: mount)
provider: google-drive
connection: conn_01JA...      # OAuth connection of Mari, tokens kept in OpenBao
root: "My Drive/Work"         # optional sub-folder
mode: read-write              # read-only | read-write
cache: metadata               # metadata | metadata+thumbnails | full-copy
```

How it works:

- **Connection**: the user connects the account with OAuth. The platform stores the refresh token
  encrypted in OpenBao, never in the database. The user can disconnect at any time; the mount then
  shows "disconnected".
- **Items get platform node IDs** on first sight: `mount_items(node_id, mount_id, external_id,
  etag)`. So links, tags, comments and search work on mounted items too. If the item is deleted
  outside, links to it break and the UI hides them.
- **Change tracking**: the provider polls or subscribes to the external change feed (Google Drive
  `changes`, Microsoft Graph `delta`, Dropbox `list_folder/continue`) and writes journal entries.
  Then thumbnails, the photo timeline and search can include mounted items if the user wants.
- **Content stays outside** by default. Reads stream through the platform. With `cache:
  full-copy`, the platform keeps a copy with versions, and then it is also a backup of the
  external drive (a feature people pay for).

The hard part is **permissions**. Every access to the mount uses Mari's external credentials:

- A mount is **personal by default**. Nobody else can open it, also not tenant admins (they can
  only see that it exists and remove it).
- **Sharing a mount** (or an item in it) means "other people use Mari's Google account". This needs
  an explicit choice by Mari, is read-only by default, shows a warning, and can be forbidden by
  tenant policy. Prefer the alternative: an organisation mounts a **shared** source with an
  organisation connection (a service account or a SharePoint site), owned by a group.
- The external system's own rights still apply: the platform can never give more than the
  connection allows. The effective right is the platform right ∩ what the external account can do.
- **Agents** can use mounts only when the agent's scope includes the mount node. Mounts are not in
  the default scope of a new agent.
- **Moves and copies across the boundary** are real transfers (upload or download), with progress,
  not a metadata change.

Tenant admins choose which mount providers are allowed. Some organisations will forbid personal
Google Drive mounts to stop data leaving the company.

### 7.5 Other Ruum instances and federation (later)

Ruum is open source (Apache 2.0), so anyone can run an instance. Instances can work together at two
levels. Both sides share the node model, so versions, tags, links, comments and the journal map one
to one, which no other storage service gives.

**Level 1: mount another instance where you have an account.** A mount provider of 7.4 for Ruum: the
person connects their account on the other instance with OAuth (instances already speak OAuth 2.1
with dynamic client registration, for MCP clients). The mount follows the other instance's
**changes feed** with a cursor (section 2.5), so versions, tags and comments come through natively,
and diff plugins work. The rules of 7.4 apply: personal by default, the other instance's rights
always apply, content streams unless a copy is chosen.

**Level 2: federation, sharing between instances.** A person shares a folder with
`jaan@ruum.example.org`, a user of another instance.

- **The open standard, not an own protocol**: **Open Cloud Mesh (OCM)**, the protocol for sharing
  between instances that Nextcloud, ownCloud, Seafile and CERNBox use, so Ruum can also share with
  them. Ruum's own features (versions, tags, typed markers) go in an OCM extension.
- **The owning instance stays the source of truth.** Jaan's instance shows the share as a mount
  ("shared from ruum.space"); every request is checked by the owner's instance; data leaves only when
  Jaan opens or caches it; a revocation takes effect at once.
- **Federated principals**: a new principal kind `remote_user` in the authz schema, with the usual
  roles. Resharing is off by default and needs `share`. Agents on Jaan's side act for Jaan within
  their scope.
- **Trust between servers**: each instance publishes its keys and capabilities at a well-known
  address (`/.well-known/ocm` and `/.well-known/ruum`); requests are signed with HTTP Message
  Signatures (RFC 9421); each share has its own token; the recipient must accept a share (against
  spam); content from other instances is untrusted (prompt injection, malware).
- **Tenant policy**: federation off, an allow-list of instances, or open; admins can block
  instances (decision 40).
- **Files and folders first.** Databases, apps and other typed nodes show as read-only placeholders;
  federating them needs their providers to take part, later.
- **Optional: "Sign in with another Ruum".** Every instance is an OpenID Connect provider (Zitadel),
  so a person can use their `ruum.space` account on a self-hosted instance.

**Self-hosting.** Open source with federation needs a supported self-hosting package (a single-machine
Docker Compose setup, later a Helm chart) and a versioned federation protocol, so instances of
different versions can talk to each other.

## 8. Databases

### 8.1 The node

The node content is a manifest. Its versions are the history of the configuration. The data lives
in the database engine.

```yaml
# /Projects/Garden planner/garden.db   (type: database)
engine: postgres          # postgres | document | search | vector | sqlite | kv
version: "17"
plan: shared-small        # shared-small | dedicated-2cpu | ...
extensions: [postgis, pgvector]
backups: { pitr_days: 7 }
```

A control loop (the operator pattern) compares the manifest with the real database and makes them
equal. The node shows status: `provisioning`, `ready`, `failed: <reason>`.

### 8.2 Engines: Postgres for almost everything

Each extra engine is one more thing to run, back up and fix at night. Start with Postgres and its
extensions, and add engines only when customers need them:

| Type | Start with | Later, if needed |
|---|---|---|
| relational | PostgreSQL | - |
| document | PostgreSQL JSONB; FerretDB for MongoDB wire compatibility (it runs on Postgres) | - |
| full-text | Postgres FTS, or ParadeDB `pg_search` (BM25) | Meilisearch or OpenSearch |
| vector | `pgvector` | - |
| lite / embedded | SQLite file with replication to object storage (Litestream style) | - |
| key-value / cache | - | Valkey |

The **SQLite type** fits "a database is a file" literally, and it is cheap for the many small
databases that AI agents make. One writer at a time is enough for most small apps.

### 8.3 Placement

- `shared-*` plans: one Postgres **database** with its own roles inside a large shared cluster. High
  density, low cost. Limits by connection count, statement timeout and disk quota.
- `dedicated-*` plans: an own cluster managed by **CloudNativePG** on Kubernetes, with streaming
  replica and point-in-time recovery to object storage.
- Moving from shared to dedicated is a planned operation (logical replication, then switch).

### 8.4 Access with the same permission system

- No static passwords. A principal with `view` gets a read-only role. A principal with `edit` gets a
  read-write role. `manage` allows schema changes and settings.
- **Connection proxy**: clients connect with a platform token as password to
  `<node-id>.db.hel1.ruum.space` (internal only in the first version). The proxy checks the token and the role, then connects with
  a short-lived role to the real cluster. Revoke the grant, and the next connection fails.
- **HTTP query API**: `POST /v1/databases/<id>/query` with SQL and parameters. Agents and
  serverless code like this; no driver and no connection pool needed.
- **Branches** (later): a copy of a database for tests, or for an agent to try a migration. For
  small databases `CREATE DATABASE ... TEMPLATE` is enough. A branch is a child node.

### 8.5 Agents and data: read-only by default, a privacy filter, writes when granted

- **Read-only by default.** An agent session can query the databases bound to its workspace, in a
  read-only transaction, with a statement timeout, a row and size cap, and every query logged with
  `actor` and `on_behalf_of`.
- **Privacy filter.** Each column is classified `personal` (names, emails, phones, addresses, free
  text), `secret` (tokens, password hashes) or `normal`. **The agent suggests the class** of every
  new column: the agent that writes a migration puts its classification in the migration, and for
  existing data an agent proposes classes from the column names and a sample of values. The person
  confirms them in the approval they already give (the publish of the migration), as one short list
  with the suggestions filled in. Until confirmed, a column counts as `personal`, so a forgotten
  column fails safe, and the digest lists unconfirmed columns. Agents see that `secret` columns
  exist; `NULL` and empty values are shown as they are, and any other value is replaced by the
  **hidden-value marker** (below), never the value. They see
  `personal` columns as **consistent pseudonyms** (the same value always gives the same pseudonym),
  so joins, grouping and counts still work. Aggregates are allowed. Real values only through an
  explicit, time-limited, logged grant ("show personal data in this session for one hour").
  Postgres: the **PostgreSQL Anonymizer** extension masks inside the database, bound to the agent's
  database role, so nothing unmasked leaves the database. SQLite: masking in the HTTP query layer.
- **Writes in steps.** On a draft, the agent writes freely to a **database branch**, made with
  masked data by default, so previews do not copy real personal data. Schema changes are
  migrations (decision 19). Writes to the **live** database need a grant: per workspace in
  `project.yaml` or per session through a prompt, limited in time and revocable. Destructive
  statements (DROP, TRUNCATE, DELETE or UPDATE without WHERE) always ask, with the statement and the
  number of rows. The backup point in time is recorded before each granted write session.
- **Typed markers, not look-alike strings.** A hidden or pseudonymised value is a distinct typed value,
  so nobody mistakes it for real data: in JSON results (the HTTP query API, MCP) a secret is
  `{"$ruum": "hidden", "class": "secret"}` and a pseudonym is `{"$ruum": "pseudonym", "id":
  "p_8f3a91"}`; in logs and traces the same as a reserved map attribute. The UI shows them as chips
  ("hidden: secret", "person 8f3a91"). The same markers are used for columns, logs, metrics and
  traces.
- Outside agents over MCP follow the same rules. Data rows reach agents marked as untrusted, like
  log lines (section 9.6).

## 9. Web applications

### 9.1 The node

The content of a `webapp` node is a deployment archive (or a build from a repo node). **Each
version is one deployment.** Rollback is "restore version 12". The manifest is in the archive:

```yaml
# app.yaml inside the archive
runtime: node22           # static | node22 | python3.13 | container
start: "node server.js"
bindings:
  db:     { node: ../garden.db, role: edit }       # resolved to the node ID at deploy time
  photos: { node: /Photos/Garden, role: view }
secrets: [STRIPE_KEY]     # values are set by a manager, never in the archive
access: platform          # platform (sign-in + `view` on this node) | public
```

### 9.2 Identity and access

- **Who may use the app** is the `view` permission on the app node. Share the app like a file:
  with a person, a group, the whole tenant, a link, or `public`.
- The platform **gateway** checks this before a request reaches the app (the model of Google IAP
  or Cloudflare Access). It forwards a signed header (`Platform-User: <JWT>`). The app needs no
  login code. A small SDK checks the header.

### 9.3 App-defined roles

An app can declare its own roles in the manifest. The platform stores and grants them, so the
owner manages "who is an approver" in the same share dialog as "who can use the app":

```yaml
roles:
  member:   { description: "Can add plants and see the plan", default: true }
  approver: { description: "Can approve orders" }
  admin:    { description: "Can change app settings and grant app roles" }
```

- An app role always includes the right to use the app (`view` on the app node). A grant of
  `approver` alone is enough to open the app.
- `default: true` is the role of everybody who has `view` on the app node but no explicit app
  role.
- A person with `share` on the app node can grant all app roles. The app can mark one role as
  `grants_roles` (here `admin`), so the app's own admins can manage roles without platform
  rights on the node.
- In the engine this is one generic type, not one schema per app:
  `app_role:<app-node-id>/<role>#holder@user:mari`. The role names come from the deployed
  manifest. When a new version removes a role, its grants stay but are inactive, so a rollback
  restores them.
- App roles are app-level. Rights on objects inside the app (for example "Mari can edit order 17")
  are the app's own data. A later option: a small platform authz API with an app-private
  namespace, for apps that want Zanzibar-style rights on their own objects.

### 9.4 What the app knows about the user

The gateway sends a short-lived signed token with **only the facts that this app may know**:

```json
{
  "iss": "https://ruum.space", "aud": "app:01J9...garden",
  "sub": "pairwise:6f2c...",              // stable per app, not the global user ID
  "tenant": "acme",
  "name": "Mari Tamm", "email": "mari@acme.ee",   // only if the manifest asks and the tenant allows
  "roles": ["member", "approver"],         // app roles only
  "node_roles": { "../garden.db": "edit" },// effective rights on the bound nodes, for UI decisions
  "actor": "user",                          // or "agent", see section 3.4
  "exp": 1762380000
}
```

- The `sub` is a **pairwise ID**: stable for this app, different for every other app. Two apps
  cannot join their user lists without the platform.
- The app declares the claims it needs (`claims: [name, email]`). The tenant policy can refuse
  claims, for example no email for public apps.
- The token is made for each request (or cached for a few seconds), so a removed role stops working
  at once. The app should not keep its own copy of roles.
- `GET /v1/apps/self/users?role=approver` lists the holders of an app role (pairwise IDs and
  allowed claims), for screens like "assign to an approver".
- **The app is a principal.** A binding gives the `app` principal a role on the target node. The
  person who deploys must have that role on the target; this stops privilege escalation through
  apps. The grant records `granted_by`.
- The app can also act **on behalf of the user** (token exchange). Then the effective right is
  the user's right ∩ the binding. Use this when the app shows each user only their own files.

### 9.5 Runtime

AI-generated code is untrusted code. Plan for strong isolation from day one:

- Static sites: served from object storage through the gateway and a CDN.
- Dynamic apps: OCI images built with buildpacks (Railpack/Nixpacks or Cloud Native Buildpacks),
  run on Kubernetes with the **gVisor** runtime class (or Kata/Firecracker microVMs on dedicated
  servers later) in a separate node pool, with Cilium network policies: an app reaches only the
  gateway, its bound databases and the internet egress proxy.
- **Scale to zero**: most agent-built apps get few requests. Use Knative Serving or the KEDA HTTP
  add-on; first request after idle takes about 1-3 s.
- Logs, metrics and the last errors of each app are available by API. An agent needs them to fix
  its own app.
- Domains: `<app>.<tenant>.ruumapps.net` by default (the apps domain, section 17.1). Custom domains
  with automatic TLS (ACME).
- **Public apps and custom domains need approval.** Both are off for every tenant. A Ruum platform
  admin turns them on per tenant, in the admin console (section 12.3). After that, the tenant's
  own admins decide which of their apps are public.
- **The apps domain is a separate registered domain** (`ruumapps.net`), not a subdomain
  of `ruum.space`. Put it on the **Public Suffix List**, so each tenant's subdomain is its own site
  for the browser. Two reasons: a malicious app on a subdomain of `ruum.space` could set cookies
  for `ruum.space` itself, and if Google Safe Browsing flags one phishing app, it can flag the
  whole registered domain. That must never be the domain of the sign-in page. Downloads and
  previews of user files use the same apps domain (`<id>.content.ruumapps.net`).

### 9.6 Logs, metrics and errors of apps

Every app gets observability without setup by its author, and agents get it too: an agent that
learns about errors only when a person complains ships broken apps.

**What is collected:**

- **Backend:** the app's logs (standard output), request metrics (rate, latency, errors), CPU,
  memory and cold starts, traces through OpenTelemetry (the runtime adds the instrumentation),
  database slow queries, errors and failed migrations, and build logs.
- **Frontend:** a small Ruum browser script records JavaScript errors, unhandled rejections,
  console errors, failed requests and Core Web Vitals. Always on in previews. In live apps a
  **privacy-aware mode is on by default**: errors and Web Vitals only; end users only as pairwise
  pseudonyms; no IP addresses stored; URLs without query strings; no request bodies, cookies or
  form values; PII scrubbed. It needs no cookies of its own, and Ruum gives the app author a
  ready paragraph for the app's privacy notice. The **full mode** (breadcrumbs of user actions,
  failed request details, performance traces) is offered with one click at the first publish and
  in the app's settings, with a plain explanation of what it records.
- **Source maps** are uploaded at build time and kept private, so a minified stack trace shows
  `src/PlantList.tsx:42`.
- **Issues:** errors are grouped by fingerprint (type and top frames) into issues with a count,
  first and last seen, the app version where they started, and how many users they affect.

Storage: the platform's Grafana stack (Loki, Mimir, Tempo) with one storage tenant per Ruum tenant;
retention by plan (section 4.3).

**Access:** API and MCP tools to search logs, read metric summaries, list issues, read an issue
with its stack trace and recent events, and compare before and after a deploy. Scoped like
everything else; logs can hold personal data, so reading them needs `edit` on the app.

**Errors reach agents without being asked**, at three levels:

1. **A check after every build and preview**, built into the session loop (it does not depend on
   the agent remembering): a headless browser opens the main routes and collects console errors,
   failing requests, server errors and new issues. The agent gets the result as a message and
   fixes the problems before it asks the person to look.
2. **Push to the active session:** a new or regressed issue of the workspace's app reaches the
   session at the next turn ("TypeError in PlantList.tsx:42, 37 times in 5 minutes since your
   last publish"). On drafts and previews the agent may investigate and fix by itself; publishing
   needs the usual approval.
3. **A guardian when no session is open** (optional, per workspace, with its own budget): it triages
   new live errors, opens a session with a diagnosis and a proposed fix on a draft, and notifies the
   owner. It never publishes, removes duplicates and is rate-limited, so it cannot loop.

**The platform protects live apps without any agent:** if the error rate rises clearly after a
publish, Ruum rolls back to the previous version and then tells the session.

**Telemetry privacy, like database columns.** Logs, metric labels and trace attributes use the
same three data classes as columns (section 8.5), with one difference for secrets:

| Class | Columns | Logs, metrics, traces |
|---|---|---|
| `secret` | the column is visible; non-empty values become the hidden-value marker | **the value is never recorded**: a non-empty value becomes the hidden-value marker (section 8.5), at the source and again at ingestion; null and empty values are serialised by the logging library as usual |
| `personal` | pseudonyms for agents | keyed pseudonyms before storage; optionally a short-lived encrypted raw copy (below) |
| `normal` | readable | readable by agents without a special grant |

- **Analysis during development.** A privacy review agent (section 11.2) reads every log, metric and
  trace statement in the app's code and classifies each field. The result is a file in the repo,
  `telemetry-privacy.yaml`. The person confirms new fields in the publish approval they already
  give, as a short pre-filled list, as for columns.
- **Structured logs.** The Ruum logging helpers write message templates with named fields
  ("order {order_id} paid by {customer}"), so the template stays readable and each field has a class.
  Free text without a template is treated as `personal` and scrubbed (patterns and name
  recognition) before storage.
- **Pseudonyms before storage, raw values only by choice.** Storing raw personal values in logs makes
  erasure requests hard (log stores cannot delete single entries well), widens what a leak or a
  broad grant exposes, and keeps personal data longer than debugging needs. So by default a
  `personal` value is replaced before storage by a **keyed pseudonym** (an HMAC with a per-tenant
  key). Debugging still works: Ruum computes the pseudonym of a known value ("what happened to
  kati@example.com?") and finds every entry about that person, without storing the email. An app can
  also keep an **encrypted raw copy** with a short retention (7 days by default), each end user's
  values under their own key: a grant can decrypt them for a while, and deleting an end user deletes
  their key, which makes all their stored values unreadable at once (crypto-shredding).
- **Enforcement** happens twice: a lint rule in the app's checks blocks `secret` fields in log
  calls, and the ingestion pipeline (OpenTelemetry Collector processors driven by
  `telemetry-privacy.yaml`) replaces non-empty `secret` values with the hidden-value marker and
  pseudonymises `personal` ones. Unclassified
  fields count as `personal`. Metric labels with personal values are refused, because they also
  explode the number of series.
- Log lines still reach agents marked as untrusted data, because user input ends up in them.

### 9.7 Issues, and reports from app users

**Issues are nodes.** An issue is a file of type `issue` in a folder of the project (`Issues/` by
default; any folder works, like sessions). Its content is structured fields plus a text body; its
versions are its history; comments are node comments; labels are tags; a board is a smart folder
grouped by status. Issues have a status, an assignee (a person **or an agent**) and links: to error
issues of section 9.6, to the sessions that work on them, to the drafts that fix them.

**Issue templates** (section 2.7) give each kind of issue its own fields, as a JSON Schema plus a
form layout: a bug (steps, expected, actual, severity, app version), a feature request (problem,
proposal, who needs it), a question, or the tenant's own kinds. The template of an issue is
recorded in it, so old issues still read correctly after a template changes.

**One concept for problems.** A user report, an error from monitoring (section 9.6), a person and
an agent all create the same kind of issue, with a `source` field. **Every new error opens an
issue**, because one error can be serious: the first occurrence of a fingerprint creates the issue,
and every later occurrence only links to it and counts. The issue shows the count, first and last
seen, the affected users and a trend, all as fields to sort and filter by. A closed issue whose
error comes back is reopened as "regressed". An issue can be muted (it keeps counting, without
notifications). **A mute always expires**: 7 days by default, at most 30; then the issue notifies again as
usual. When one publish brings many new fingerprints at once, they are grouped under one
parent issue for that publish, so a bad release is one thing to look at, not hundreds.

**"Fix in a session".** An issue has a button that opens an agent session in its workspace with the
issue, its attachments and the linked errors as context; the session links back, and publishing
the fix closes the issue (after the person confirms, or automatically when the error stops).

**Reports from the people who use an app.** Ruum adds a **report widget** to live apps (on by
default, small, the author can move or turn it off) and an automatic prompt when the app breaks
("Something went wrong. Tell us what you were doing."). A report attaches, after the reporter sees
and can remove each item:

- a screenshot, with the same drawing tools as preview annotations (section 11.1) and a blur tool
  to hide private parts;
- the route, browser, screen size and app version;
- **the logs of this visit**, frontend and backend (linked by trace IDs): by default every log record
  of the visit is a candidate. Ruum checks them for `personal` fields and shows the records that
  have any. The reporter decides, per field or for all at once:
  - **include as it is**: Ruum decrypts the reporter's own values from the encrypted raw copy
    (section 9.6), only for this issue; possible only when the app keeps that copy;
  - **pseudonymise**: the keyed pseudonym (the default choice);
  - **remove**: replaced by the hidden-value marker with no way to recover it.

  Everything else in the record stays (time, level, message template, other fields), so it is still
  clear that a record of that kind was there. Only records of the reporter's own visit are offered;
  records about other people are never attached. The choices are stored with the issue as the
  reporter's consent.

  **Asking again later.** While investigating, a person working on the issue (or an agent, through
  that person's approval) can ask the reporter to reveal specific fields, with a short reason. The
  reporter gets the request on their receipt link, and by email if they left one, and approves or
  refuses per field. Nobody can ask again about fields the reporter refused, but the reporter can
  change their mind at any time on the receipt link and allow access after all (for example after
  pressing the wrong button). This works only
  while the encrypted raw copy exists, so filing a report **holds** the raw copies of that visit's
  records until the issue is closed, at most 30 days; then they are deleted on the normal schedule. `secret` values were never recorded, so there is nothing to choose for them;
- the reporter as a pairwise pseudonym, plus an email only if the reporter types one for a reply.

The report becomes an issue from the app's bug template, linked to the matching error group.
Reporters who are not members of the tenant get a receipt link that shows only the status of their
report. Public apps limit reports per visitor and per app, to stop spam.

### 9.8 Apps that write to files and external services (later)

**Data files.** An app can bind to a file node (a binding as in 9.1) and use a **records API** on it:
append a row, query, update, with a schema. Formats: CSV, JSON Lines, XLSX/ODS, Markdown tables. The
file stays readable and editable by people in Ruum: a contact-form app appends to `Leads.csv`, and
the owner opens that file.

- Ruum serialises the writes to one file and merges appends into one version per time window
  (section 5.3), not one version per row. When a person edits the file while the app writes, the
  row-level three-way merge plugin (section 5.4) resolves it.
- Above a size or write rate (for example 100,000 rows), Ruum suggests **"graduate to a database"**:
  one click imports the file into a Postgres table and can keep the file as a synced export.
- SQLite already puts a database in a file, but a binary one that people cannot read or edit. The
  next step after data files is a native **table** node type: a spreadsheet grid for people, a table
  API for apps, export to CSV and XLSX, stored in SQLite or Postgres underneath.

**Connectors to external services.** An app binds to an external resource through a **connection**
(OAuth, the same mechanism as mounts in 7.4): a Google Sheet, Airtable, Notion, a Slack channel, a
webhook.

- **One records API.** A Google Sheet inside a mounted Google Drive is a node; binding to it gives the
  same records API as a local file, and Ruum translates the calls to the Google Sheets API. The app
  code is the same for a local file and a Google Sheet.
- **The app never holds the credentials.** They stay in OpenBao; Ruum makes the calls. Writes go
  through an outbox with retries, so an outage of the other service loses nothing. Every write is
  logged with `actor` and `on_behalf_of`.
- The owner of the connection decides whose account is used (a personal one, or an organisation
  account owned by a group). Tenant admins choose which connectors are allowed.
- **Data leaving Ruum** is checked against the data classes (section 8.5): writing a `personal`
  field to an external service shows a warning, and the publish approval asks for confirmation.
- Connectors are providers (section 7.1); later they can wrap existing MCP servers to cover many
  services quickly.

### 9.9 Public projects

A person can make a project public so that others can use the app, read its source and inspect its
data, without writing anything through Ruum. The audience is `public` (anyone, no sign-in) or
"anyone signed in to Ruum", with the role `view`: the app can be used (its default app role), the
repo can be read (code, history, diffs), and the database answers read-only queries through the
HTTP query API. Public apps need platform admin approval (decision 22).

**Secrets stay hidden by construction.** App secrets are never in the code or the archive; the
manifest lists only their names and how each one is made, and the values are **write-only** in
OpenBao (nobody reads them back; only the running app gets them). Three kinds:

1. **Generated** (most of them): session signing keys, encryption keys, webhook secrets, keys between
   the app's own parts. The manifest says `SESSION_KEY: { generate: random-32-bytes, rotate: 90d }`;
   Ruum creates the value, stores it, rotates it on schedule and never shows it.
2. **From a connection**, where possible: a third-party service connected with OAuth (section 9.8),
   so nobody copies a key.
3. **Entered**, only when there is no other way (a service that issues only a key string): a person
   with `manage` on the app (or an agent with that person's approval) pastes it once.
 There is no database password at
all: the app reaches its database as the `app` principal through its binding, and the proxy gives it
a short-lived role. A viewer sees the binding in the manifest, which is a right of the app, not a
credential.

**A public project publishes a chosen set, not the whole folder.** Grants only add rights
(decision 8), so a public grant on the project folder would expose agent sessions (transcripts can
hold personal data or pasted secrets), the agents' memory, issues with reporters' attachments and
logs. "Make public" therefore offers a checklist (the app, the repo, the database, optionally the
issues) and keeps sessions, agent memory, logs and report data private. **Public projects wait for
restricted folders** (decision 8), so the private parts are protected by the tree, not by care.

**Databases are not public by default.** The owner chooses tables or views for a **public schema**;
the public sees nothing else. `personal` columns are fully hidden for the public (the hidden-value
marker), because consistent pseudonyms would still let outsiders link records. Public queries have a
short timeout, a row cap, a rate limit per visitor and caching.

**Writes through the app are the app's business.** Visitors cannot write through Ruum (API, repo,
database), but the app itself has write access through its binding, so a form in the app can still
store what visitors enter, as the app's code allows. An app that must be read-only for the public
checks the visitor's app role (section 9.3).

**Remix** (backlog): a visitor copies a public project into their own Ruum: the code, the schema and
masked sample data, never the secrets or the real data (built on project templates, section 2.7).

### 9.10 Apps calling apps

An app is a principal and a binding gives it rights to a node (9.2), so calling another app is one
more kind of binding, with the same checks as a database binding.

```yaml
# the called app: app.yaml
apis:
  orders:
    visibility: internal        # internal: only apps bound to it | external: users, agents, partners
    protocol: http              # http (OpenAPI file) | events (AsyncAPI file)
    spec: ./api/orders.openapi.yaml
    scopes: [orders.read, orders.write]   # app-defined, like the app roles of 9.3

# the calling app: app.yaml
bindings:
  shop: { node: ../shop-app, api: orders, scopes: [orders.read] }
```

**Security, the same as for databases:**

- **No privilege escalation:** the person (or agent) who deploys the calling app must be allowed to
  grant those scopes (`manage` on the called app, or an API role that may grant them). The grant
  records `granted_by`.
- **Default deny on the network:** app sandboxes reach each other only along a binding (Cilium
  policies). Internal APIs are never reachable from the internet.
- **Identity on every call:** the platform adds a signed header with the calling app and, when
  delegated, the user it acts for: the same token format as for users (9.4), checked with the same
  SDK.
- **Delegation chains:** user → app A → app B. B sees actor = A and on_behalf_of = the user; the
  effective right is the user's right ∩ A's binding ∩ B's API scopes. The chain length is capped;
  every hop is in the audit log and the trace.
- **Data classes travel with the data:** personal and secret fields of API payloads follow 8.5 and
  9.6 in logs and traces.

**Internal and external APIs:**

- **Internal**: only bound apps of the same tenant, at an internal address
  (`http://<binding>.<api>.ruum.internal`).
- **External**: published through the app gateway (apps domain or custom domain), for signed-in Ruum
  users with app roles, agents with OAuth or agent tokens (Ruum can publish an app's OpenAPI as MCP
  tools, so any agent can use the app), partners with API keys managed by Ruum (per consumer, rate
  limits, revocation), or the public.
- Calls between tenants only through external APIs (or later federation, 7.5).

**Synchronous and asynchronous:**

- **Synchronous**: HTTP request and response, streaming and WebSockets, through the internal gateway
  with timeouts, retries for safe methods, rate limits and circuit breaking; one OpenTelemetry trace
  across the apps (9.6).
- **Asynchronous**: events on the platform bus (NATS JetStream). An app declares the topics it
  publishes; another app subscribes through a binding with a grant. Delivery is at least once, with
  retries, a dead-letter queue and replay; events carry actor and on_behalf_of. Apps can also
  subscribe to Ruum's own events for nodes in their scope ("a file was added to Invoices/").

**Contracts between apps:** each API is described in OpenAPI (synchronous) or AsyncAPI
(asynchronous) in the app's repo. Ruum generates a typed client for each bound app, and the app's
development pipeline (11.2) runs a breaking-change check when other apps depend on the API, so an
agent changing one app cannot silently break another.

## 10. Source code and builds

Store code in **Git**, even if the user never sees Git. Do not invent a new version control system.

- A `repo` node is a provider. The first engine is **Forgejo** (open source, Gitea family, good
  API, Actions runner compatible with GitHub Actions) running behind the platform, with platform
  auth. A second provider links a GitHub repository (GitHub App) for developers.
- **The simple view for non-developers**: "Save" makes a commit. An agent's work goes to a
  **draft** (a branch) with a preview deployment. "Publish" merges and deploys. History is a list of
  saves with plain-language summaries (written by the agent). No pull request words.
- **Builds** are not general CI. A build is: install, test, package, then upload the archive as a
  new version of the `webapp` node, as a **preview** first. This covers almost all needs of
  non-developers. Run builds in the same sandboxed pool as the apps.

## 11. Making it good for AI agents

- **One API, three faces**: REST with an OpenAPI description, an **MCP server** (remote, OAuth),
  and a CLI. Every action in the UI is in the API.
- **Declarative**: a project is a folder with a `project.yaml` that names its app, databases and
  bindings. `apply` makes the platform match it. Agents are good at writing manifests and bad at
  long click paths.
- **Safe by default**: everything has versions, so every agent action can be undone. Previews and
  database branches let an agent try before it publishes.
- **Own identity, narrow tokens**: an agent has its own credentials (section 3.4) and a scope of
  one project folder, a list of roles and an expiry. Users see "Agent X acting for you" in the
  audit log and on every version it made.
- **Budgets and quotas** per agent and per project (compute, storage, number of databases, egress),
  with a hard stop. Agents in a loop create resources fast.
- **Good errors**: machine-readable error codes with a hint for the next step.
- **Templates** (section 2.7): "web app + Postgres + sign-in" as a ready project that an agent copies.

### 11.1 Agent workspaces (agent sessions inside Ruum)

People should be able to change their apps from inside Ruum: open the app, open its agent, type the
change, look at the preview, press Publish. Two concepts make this work, and both are nodes, so they
get versions, sharing, scope and the agent identity of section 3.4 without new mechanisms:

| | Workspace | Session |
|---|---|---|
| What | the **project folder** (`type: project`) with agent settings; any folder can become one ("Use agents here") | a file in it (`type: agent_session`) |
| Holds | defaults (harness, model), instructions (`AGENTS.md`), the scope (the folder itself), budget, approval rules, the environment (sandbox image, repo, bindings, secrets), **shared memory** that all sessions read and add to | one conversation: its transcript (versioned), its own draft branch and preview, its token use |
| Lifetime | as long as the project | short or long; can be closed, reopened or forked |

```yaml
# /Projects/Garden planner/project.yaml   (the workspace settings)
agents:
  harness: claude-agent-sdk   # or codex, gemini-cli, opencode ... (harness providers)
  model: claude-sonnet-5-5
  instructions: ./AGENTS.md
  memory: ./.agents/memory/   # shared by all sessions of the workspace
  budget: { tokens_per_day: 2000000 }
  approvals: { publish: ask, delete: ask, share: never }
```

```yaml
# /Projects/Garden planner/Sessions/Sortable plant list   (type: agent_session)
started_by: mari
draft: drafts/sortable-plant-list
harness: claude-agent-sdk     # the workspace default unless changed
```

- **The session node.** The transcript is the node's content, with versions, so a session can be
  shared, read later, or forked. Who can `edit` the session can type into it; `view` can only read
  it. Who can `edit` the workspace can start sessions in it.
- **Parallel sessions.** Each active session has its own sandbox and its own draft, so two sessions
  ("fix the sign-in bug", "redesign the plant list") do not clash, and each is published on its own.
- **Organising sessions.** Sessions are ordinary nodes, so folders, tags, search, shortcut links,
  smart folders, sharing and trash work on them as on any file. `Sessions/` is only the default place
  for new sessions. A session belongs to its **nearest ancestor that is a workspace**, so it can sit
  in subfolders at any depth. Moved into another workspace, its next turns use that workspace's
  settings and scope (after a warning); the old transcript stays as it is. Moved outside every
  workspace, it becomes a read-only archive that can be read and forked. A shortcut to a session
  does not change its workspace.
- **The identity.** A session runs as an agent principal that acts for the person who types, with
  the session's scope. Every change shows "Garden agent acting for Mari", and "undo what this agent
  did" works (section 3.4).
- **"Open agent" on any node.** It starts a session in the node's workspace, or reopens the last one,
  with the context ready: the repo checkout and its `AGENTS.md` / `CLAUDE.md`, the app manifest and
  bindings, recent logs and errors of the app, the database schema, recent changes from the
  journal, and the node the person started from.
- **Drafts and previews.** The agent works on a draft (section 10) and shows the preview deployment
  beside the chat. "Publish" merges and deploys. Nothing reaches the live app without a publish
  (approval rules in the manifest).
- **The runtime.** One sandbox per active session (from the workspace's environment) in the tenant app pool (gVisor, scale to zero,
  section 9.5): a checkout of the repo node, the `ruum` CLI and MCP server preconfigured with the
  session's agent token, internet only through the egress proxy. An idle session sleeps; the
  transcript stays in the node.
- **Data access** (section 8.5): read-only with a privacy filter by default; writes on database
  branches, and on the live database only when granted.
- **Errors come to the agent** (section 9.6): a check after every build and preview, new issues
  pushed to the active session, and an optional guardian when no session is open.
- **The UI.** A chat view in the web UI that shows the harness's event stream: messages, tool
  calls, diffs, approval prompts, and the preview. A terminal tab (xterm.js) for developers.

**Preview annotations.** People give feedback on the UI by pointing and drawing, not only by
typing, and the agent gets that feedback with its context. Two modes in the preview pane:

1. **Point at an element** and attach a comment. The agent gets a stable selector, the role and
   accessible name, a short HTML snippet, the key computed styles, the size and position, the
   route and viewport, the console errors of that moment, a cropped screenshot and **the source
   location** (`src/PlantList.tsx:42`). Preview builds add a build plugin that marks each element
   with its source file and line; live builds never have it.
2. **Draw on the screen**: Ruum captures exactly what the person sees and opens it in a drawing
   layer with freehand lines, rectangles, ellipses, arrows, text attached to shapes, colours, an
   eraser and undo (**Excalidraw**, MIT licence, embedded). The agent gets the composite image and
   the drawing as shapes with their text. When capture in the browser fails, Ruum renders the same
   route and viewport in a headless browser.

How it works:

- A small **annotation script is added to preview pages only** (never to live apps) by the app
  gateway. It talks to the Ruum UI with `postMessage`. Previews run on the apps domain, so the script
  cannot reach Ruum's own origin.
- **Annotations are comments** anchored to a preview version, a route and an element or a drawing.
  They are stored in the session, with its permissions. Anyone with the `comment` role can annotate,
  so a client can mark up a shared preview and the agent sees it.
- Several notes can be sent as **one batch**; each turn carries its annotations as images and data.
- **Status loop**: when the agent says it fixed a note, the note becomes "fixed?". On the next
  preview Ruum finds the same element again and shows the note there; the person confirms or
  reopens it.
- **Outside agents** get the same context through MCP tools: list preview annotations, resolve an
  annotation.
- **Screenshots can hold personal data**: they are nodes with the session's permissions.
- **Comments from other people are input, not commands**: the agent sees who wrote each one, and
  publishing still needs approval (prompt injection, as with any shared file).

**Harnesses are providers** (section 7.1). The UI talks to every harness through one protocol,
the **Agent Client Protocol (ACP)**, the open protocol between editors and coding agents; Claude
Code, Codex and Gemini CLI have ACP adapters. The first harness is the **Claude Agent SDK**
(Anthropic's library with the agent loop and tools of Claude Code, made for building products).
The Claude Code CLI itself can run in the terminal tab, after a check of Anthropic's terms for
hosting it for other people.

**Anthropic's own web views cannot be embedded.** Claude Code on the web (claude.ai/code) and the
Claude apps run on Anthropic's side, with the person's own Claude login; claude.ai does not allow
framing, and Ruum must not handle a person's Claude credentials. Ruum offers link-outs instead:

- for a repo linked to GitHub (the GitHub provider of section 10), "Open in Claude Code on the
  web", which works on that repository in Anthropic's cloud;
- for developers, "Open in Claude Code" on their own machine: a ready command with the Ruum MCP
  server and a scoped agent key.

**Who pays for the model** (decision 38): third-party products may not offer "sign in with your
Claude subscription" (Agent SDK terms, unless Anthropic approves it). Ruum offers both: the person's
own API key first (kept in OpenBao; no billing needed), then Ruum pays the model provider and bills
the use through the AI gateway (decision 24), metered per session.

**Harness order** (decision 39):

1. Inside Ruum, the **Claude Agent SDK**, with the ACP layer in place.
2. **Every outside agent over MCP**: ChatGPT, Claude.ai, Claude Code, Codex and others connect to
   Ruum's remote MCP server and use the person's own subscription; Ruum runs and pays for no model.
   For app work, the MCP tools cover the whole loop: edit code in the repo, run a build, open a
   preview, publish (with the same approvals as in a session).
3. Soon after, **OpenAI Codex inside Ruum** (open-source CLI and SDK, with an ACP adapter), so that
   non-technical people who use ChatGPT get the same one-screen experience without setting up a
   connector. It starts with the person's own OpenAI API key. Codex can also sign in with a ChatGPT
   plan; whether a hosted product may use that is the same terms question as for Claude, so it is
   not used without a check.

**Risks.** Prompt injection: the agent reads files, also files that other people shared, and they
can contain instructions for it. The scope is the main limit (a session sees only its project),
plus approvals for publish, delete and share, and no secrets beyond its bindings. Cost: a budget
per session and per day with a hard stop.

### 11.2 The development system for users' projects

Ruum gives every project the same kind of development system that builds Ruum itself (section 18),
running in the background: the person describes a change, and a set of agents builds, checks and
reviews it before the person sees the result. Ruum sets the default; a project template can bring
its own; a person can change it later in `project.yaml` or in the project settings.

```yaml
# project.yaml
agents:
  pipeline:
    on_change:                       # every draft of a session
      - tests        # a test-writer agent adds or updates tests for the change
      - checks       # build, lint, tests, the post-build browser check (section 9.6)
      - review       # a reviewer agent with fresh context reads the diff
      - security     # only when auth, data access or secrets change
      - privacy      # classes for new columns and telemetry fields (sections 8.5, 9.6)
      - hygiene      # structured logging and parameterised SQL (below)
      - ux           # heuristics and accessibility on screenshots of changed screens
    on_publish:
      - journeys     # synthetic users run the key journeys of the project
    gate: must_pass  # must_pass | warn
    budget: { tokens_per_day: 3000000 }
```

- The results appear as review comments on the draft, with a summary in the session. The session
  agent fixes what the reviewers find before it asks the person to look.
- `gate: must_pass` blocks the publish until the checks pass; the person can override with a reason
  (recorded).
- **Cost.** Background agents use tokens. The default is light: tests, checks and one review on every
  change, the specialist reviews only when their area changes, cheaper models for routine checks.
  The budget is per project, with a hard stop. Who pays follows decision 38.
- The pipeline stages are providers, so a team can add its own (for example a reviewer that knows
  its domain rules).
- **Structured logging and parameterised SQL are required, not only advised.** The default
  `AGENTS.md` says so; lint rules in the `hygiene` stage block log messages built by string
  concatenation or interpolation (use message templates with named fields) and SQL built from
  strings (use bound parameters or a query builder); the reviewer checks it too. Parameterised SQL
  also prevents SQL injection, and query logs then store the statement template with classified
  parameters, never raw values.

## 12. Architecture

```
                      Internet
                         |
               Hetzner Load Balancer / CDN
                         |
     +-------------------+--------------------+
     |                                        |
  Platform gateway (Envoy)               App gateway (separate domain)
  web UI, REST, MCP, uploads             auth check -> tenant apps
     |                                        |
     v                                        v
  Core service (Rust, modular monolith)   Tenant app pool (gVisor, scale to zero)
   - nodes, versions, tags, journal           |
   - sharing / authz calls  ---------> SpiceDB|
   - blob API  ----------------------> Object storage (S3 API)
   - provider registry, webhooks              |
   - tenants, quotas, metering         DB proxy -> Postgres clusters (CloudNativePG)
     |                                             shared + dedicated
     | outbox -> NATS JetStream
     v
  Workers: media (libvips, ffmpeg), search index, webhooks,
           builds, DB/app reconcilers, GC, backups
     |
  Zitadel (identity, SSO, SCIM)    OpenBao (keys, secrets)    Grafana stack (logs, metrics, traces)
```

### 12.1 Stack

| Area | Choice | Why |
|---|---|---|
| Core services | **Rust** (axum, tokio, sqlx) | The team already runs this stack in `kadakas-server`. Low memory and cost, good for the blob path. |
| Web UI | TypeScript + React (or SvelteKit) | Large ecosystem; AI agents write it well. The Kadakas editor stays Rust/WASM. |
| Metadata | **PostgreSQL** (CloudNativePG) | One engine for nodes, journal, outbox, tags, PostGIS, pgvector. |
| Authorization | **SpiceDB** | Zanzibar model; inheritance, groups and links without custom graph code. |
| Identity | **Zitadel** | Multi-tenant, OIDC + SAML + SCIM + passkeys, self-hosted, EU-friendly. |
| Events | Postgres outbox -> **NATS JetStream** | Outbox for correctness; NATS is much lighter to run than Kafka. |
| Object storage | Hetzner Object Storage -> self-hosted **Ceph RGW** (or Garage) later | S3 API everywhere (`object_store` crate), so the change is configuration. Avoid MinIO: its open-source edition has been cut back. |
| Orchestration | **Kubernetes** on Talos Linux | Needed for tenant apps, CloudNativePG and scale to zero. Talos is minimal and declarative. |
| App isolation | gVisor runtime class, Cilium policies | Untrusted code. |
| Secrets and keys | OpenBao | Tenant keys, app secrets, DB role leases. |
| Observability | OpenTelemetry -> Grafana, Loki, Mimir/Prometheus, Tempo | Open source, runs on the same cluster. |
| Billing | Stripe + metering from the journal (or Lago, open source) | Usage-based plans. |
| Search (file text) | Tantivy (Rust) or ParadeDB | Later phase. |

Build the core as a **modular monolith**: one deployable with clear modules and internal
interfaces. Split a module into its own service only when it needs to scale on its own (the blob
path and the workers will be first).

### 12.2 Local development

The whole system runs on a laptop (macOS or Linux, about 8 GB free memory) with Docker Compose,
the same way as `kadakas-server` (`scripts/dev.sh up`). All parts have arm64 images.

| Production | On the laptop |
|---|---|
| Kubernetes (Talos) | Docker Compose; optionally a local `kind` or `k3d` cluster to test the Kubernetes manifests |
| CloudNativePG | one Postgres 17 container (with PostGIS and pgvector) |
| Hetzner Object Storage | Garage (single node), with the same S3 API |
| Zitadel, SpiceDB, NATS, OpenBao, Forgejo | the same containers, OpenBao in dev mode |
| Email | Mailpit (catches all mail) |
| Load balancer, TLS | Caddy with a local certificate for `ruum.localhost` and `*.ruumapps.localhost` (`*.localhost` names point to the laptop without DNS) |
| Tenant apps in gVisor | plain Docker containers through the same **runtime driver** interface; gVisor and scale to zero are tested on the staging cluster |
| Google/Microsoft sign-in | Zitadel local users and passkeys; a mock OIDC provider for SSO tests |

- `scripts/dev.sh seed` makes two tenants, three users, an agent key and sample files, so the
  tenant isolation tests and manual checks have data.
- The MCP server is at `https://ruum.localhost/mcp`, so Claude Code on the same laptop can use the
  local Ruum like a customer's agent.
- The core service also runs without Docker for unit tests, with in-memory implementations behind
  the storage traits (as `memory.rs` in `kadakas-server`). One conformance suite runs against both.
- Staging on Hetzner (a small cluster in Helsinki) is the place for the parts that a laptop cannot
  show: gVisor, scale to zero, failover, backups and restores.

### 12.3 User interface and admin console

The first web UI is small, because agents use the API. It has:

- sign-in, account and passkeys;
- the file browser: folders, upload and download, move, rename, trash, tags, search by name;
- the share dialog and "Shared with me";
- version history with diff and restore;
- agents: create, scope, keys, activity, "undo what this agent did";
- database and web app pages: status, logs, app roles, bindings, deployments and rollback;
- tenant admin: members, invitations, plans and limits, audit log.

Not in the first UI: the photo timeline, rich previews for every type, offline use.

The **Ruum admin console** is separate, for platform admins only: tenants, plans, approval of public
apps and custom domains, abuse reports and takedown, and system health.

### 12.4 Localisation

The UI is localisable from the start of the work on it (decided 2026-11-07, provisional):

- **FormatJS (`react-intl`) with ICU MessageFormat**, so plurals, cases, dates and numbers are right in
  every language (Estonian needs plural and case forms that string concatenation cannot make).
- **Languages:** English is the source; **Estonian** is the first translation. The list of languages
  is configuration.
- **Every UI string is in a message catalogue** (`web/src/locales/<lang>.json`, keys by screen and
  meaning). A lint rule fails the build on literal text in components.
- **A pseudo-locale** (accented, about 40% longer text) runs in the browser tests and the visual
  tests, so untranslated strings and layouts that break with longer text are caught.
- **Locale choice:** a user preference stored with the account (an additive contract change), then
  the browser's language, then English. Dates, times, numbers and sizes use `Intl` with that locale.
- **Server texts:** API errors keep their stable code; the UI shows a localised text per code and
  falls back to the server's English `message`. Emails and invitations are localised on the server
  by the recipient's language. MCP tool descriptions stay in English (for agents).
- **Right-to-left ready:** CSS uses logical properties (`margin-inline-start`, not `margin-left`).
- **Translations** are made by a translator agent and checked by a second agent; until a person
  checks them they are marked "machine translated" in the catalogue metadata.

### 12.5 The design system in code

The look of Ruum is defined in one place, so it can be changed later without touching screens
(decided 2026-11-07, provisional). `docs/ux/design-system.md` describes it; the code enforces it:

- **Design tokens are the single source**: colours, type, spacing, radius, shadows and motion, in
  one token file in the W3C Design Tokens format (`web/src/design/tokens/`). Style Dictionary
  generates the CSS custom properties (and TypeScript constants where code needs a value). Light and
  dark are two token sets; a new brand or theme is another set, with no change to components.
- **One component library** in `web/src/ui/` (button, dialog, table and file list, form fields,
  toast, breadcrumbs, menus, chips, empty and error states, ...), each with all its states and
  accessibility built in. Screens compose these components and contain no styling of their own.
- **Lint enforces it**: no colour, length or font value outside the token files, no inline styles,
  no styling in screen folders, no literal UI text (section 12.4).
- **A component catalogue** (Ladle, a light Storybook) shows every component in every state, in
  light, dark and the pseudo-locale. Visual and accessibility tests run per component, and the UX
  review panel (section 18.9) reviews the catalogue.

Order of work: the design system first, then localisation (12.4) on top of it, so each component is
reworked once.

## 13. Hetzner

Hetzner is a good fit for the start: low prices, traffic included in dedicated servers, cheap large
disks, and data in the EU. **Ruum starts in Helsinki** (`hel1`: cloud servers, dedicated servers and object storage in one place, data in Finland). Things to plan for:

- **No managed Postgres and no managed Kubernetes** that you can depend on. You run them yourself
  (CloudNativePG and Talos make this possible). This means **operations**: when a disk fills up or
  a database server dies at 3 a.m., somebody gets an alert and fixes it. For the first version:
  automatic failover (CloudNativePG promotes the replica by itself), alerts to a phone (Grafana
  OnCall or similar), a written runbook for each alert, and **no 24/7 promise** to customers
  ("best effort" support until there is a team that can take turns on call). Section 19 replaces
  the phone alerts with agent operations: alerts go to the on-call agent, and only red actions and
  security incidents reach a person.
- Use **Cloud servers** for the control plane and spiky work, **dedicated servers** (AX for compute,
  SX for storage) for the database clusters and, later, Ceph. Connect them with vSwitch.
- Hetzner Object Storage is young. Measure its throughput and limits for your workload, and keep the
  S3 abstraction strict so you can move to Ceph on SX servers.
- **Abuse**: hosting user-made web apps brings phishing and malware reports. Hetzner acts on abuse
  reports quickly and can block servers. Have an abuse process (report form, fast takedown, URL
  scanning of new apps, no anonymous free hosting of public apps) before public apps are allowed.
  In Ruum, public apps and custom domains are **off for every tenant** until a Ruum platform admin
  turns them on for that tenant (section 9.5).
- **Backups and single provider risk**: the first backup copy goes to Hetzner Object Storage in
  another site (Falkenstein, Germany). The second copy goes to another provider, for example
  UpCloud (Finnish, with object storage in Helsinki) or Scaleway. Keep the infrastructure as code,
  so a second provider is a known path.
- A CDN in front (for example Bunny, which is EU-based, or Cloudflare) for thumbnails, static apps
  and DDoS protection.

## 14. The scaling path

Design rules that keep each step possible:

- `tenant_id` in every row and every object key; no cross-tenant joins; UUIDv7 IDs.
- Services talk through APIs and events, never through another module's tables.
- The S3 API for all blobs; a stable token format; idempotency keys on all writes.

| Stage | Size (rough) | Shape |
|---|---|---|
| 0. Start | up to ~1,000 users | One region (Helsinki). 3-node Kubernetes (Talos). One CloudNativePG cluster (primary + replica) for metadata, SpiceDB and Zitadel. Hetzner Object Storage. One core service + workers. A few hundred euros per month. |
| 1. Growth | ~10k-50k users | Separate node pools (platform, tenant apps, databases). Read replicas. CDN. Dedicated servers for tenant databases. Blob path as its own service. |
| 2. Cells | ~100k+ users | A **cell** is a full copy of the stack (metadata DB, workers, app pool) for a set of tenants. A small global **directory** maps tenant -> cell and region. Add cells to grow. Large customers get a dedicated cell. Own Ceph cluster for blobs. |
| 3. Regions | international | Cells in more regions (Hetzner US or Singapore, or another provider). Tenant chooses its region. |

Cells are better than one large sharded database (Citus and so on) for this product: a failure
stays inside one cell, the code stays simple (each cell is "the small system"), and moving a tenant
between cells is a tenant export and import, which you need for data portability anyway.

## 15. Security notes

- Tenant isolation is tested by automated tests: for every API, a request with tenant B's token must
  never return tenant A's data. Run these on every change.
- Separate domains for user content (downloads, previews, apps) and the platform UI.
- Downloads of user files from a sandbox domain with `Content-Disposition: attachment` and a strict
  CSP, so an uploaded HTML file cannot run as the platform.
- Audit log per tenant (from the journal and auth events), exportable.
- GDPR: data processing agreement, data in the EU, export and delete per user and per tenant, opt-in
  for face grouping.

## 16. Order of work

This is several large products in one (Drive, Photos, a database host, an app host, a code host).
The order below builds the platform for AI agents first: an agent can store files, make a
database, deploy an app on it and keep the source in a repo before the consumer features arrive.
Each step is useful alone.

1. **Core**: tenants, sign-in (Zitadel), nodes, upload and download, versions, trash, tags,
   sharing, journal, agents with their own identity, web UI, REST API and MCP server.
2. **Databases**: Postgres (shared plan) and SQLite, the connection proxy and the HTTP query API.
   This step also defines the **provider contract** (section 7.1), because a database is the first
   typed node.
3. **Web apps**: static first, then containers in gVisor with scale to zero; the gateway with
   platform sign-in, app roles and bindings to databases; logs, metrics and errors (section 9.6);
   telemetry privacy classes; issues, issue templates and the report widget (section 9.7); project
   templates (section 2.7); generated secrets (section 9.9).
4. **Repos and builds**: Forgejo provider, drafts and previews, the GitHub provider.
5. **Agent workspaces** (section 11.1) and the development system for projects (section 11.2):
   workspaces (project folders) and sessions as nodes, the chat UI with previews, the
   Claude Agent SDK harness, ACP for other harnesses, the link-outs. An "Open in Claude Code"
   command for developers can come earlier, in step 1 or 2.
6. **Kadakas CAD** as the first external provider (section 7.3).
7. **Photos**: pipeline, timeline view, albums as `member` links.
8. **Desktop sync client** and mobile auto-upload. Sync is the hardest part of Dropbox; do it when the
   journal and the delta API are stable. Until then, web, API, MCP and a mount through WebDAV are
   enough.
9. **Mounted external drives** (section 7.4): Google Drive and OneDrive first, then S3 and WebDAV;
   then other Ruum instances and federation with OCM (section 7.5), with the self-hosting package;
   then data files, native tables and connectors for apps (section 9.8).
10. **Encrypted vaults** (section 5.5), together with or after the native apps. Get an external
   cryptography review before launch.

Version and diff plugins (sections 5.3 and 5.4) grow along the way: the plugin runtime, text,
JSON/XML and image diff in step 1; syntax-tree diff for code with the repos in step 4; the Kadakas
drawing diff in step 6; DOCX/XLSX redlines and three-way merge with the sync client in step 8.

**After restricted folders:** public projects (section 9.9).

### Backlog (not scheduled)

- **Remix** of public projects (section 9.9).

## 17. Decisions

Decided on 2026-11-06 for steps 1-4. "Default" means the recommended answer was taken.

The step 1 contract decisions (roles and the `share` permission, tenant header, team roots, MCP tool list, WebDAV app passwords) are in `docs/decisions/0001-step1-contract-decisions.md`.

### 17.1 For all steps

| # | Question | Decision |
|---|---|---|
| 1 | Name and domains | **Ruum**, `ruum.space`. User apps and user content go on a **separate registered domain** `ruumapps.net` (free on 2026-11-06; also take `ruumapps.com` against copycats), on the Public Suffix List (section 9.5). |
| 2 | First customers | Individuals and small teams who build with agents. |
| 3 | First agent clients | Remote MCP with OAuth (Claude, ChatGPT), plus a CLI. |
| 4 | Free tier | Yes, without public apps. Public apps are off for every tenant until a Ruum admin turns them on. |
| 5 | Region and operations | Helsinki (`hel1`). Backups to Falkenstein and a second provider. Automatic failover and phone alerts; no 24/7 promise at first (section 13). |

### 17.2 Step 1: Core

| # | Question | Decision |
|---|---|---|
| 6 | Sign-in | Passkeys, Google and Microsoft. SAML and SCIM later. |
| 7 | Tenants | Personal accounts and small teams. |
| 8 | Inheritance | Only add rights in step 1. The target is the Google Drive model, including restricted folders later. |
| 9 | Sharing outside the tenant | Default: guests and links yes, public files later. |
| 10 | Limits | The proposal in section 4.3, to be adjusted. |
| 11 | Web UI | The small UI of section 12.3. |
| 12 | WebDAV | Yes. |
| 13 | Team agents | Default: only agents owned by a person. |

### 17.3 Step 2: Databases

| # | Question | Decision |
|---|---|---|
| 14 | Engines | Postgres (shared plan) and SQLite. |
| 15 | Access from outside | Not in the first version. Only Ruum apps and the HTTP query API. |
| 16 | Dedicated plans | Later. |
| 17 | Backup promise | Point-in-time restore for 7 days. |
| 18 | Branches | Yes, for small databases. |
| 19 | Schema changes | The platform runs the declared migration at deploy time and rolls back if it fails. |

### 17.4 Step 3: Web apps

| # | Question | Decision |
|---|---|---|
| 20 | Runtimes | Static and Node.js first, then Python. |
| 21 | What apps can do | Default: HTTP, WebSockets, scheduled jobs; uploads through the blob API. |
| 22 | Public apps and custom domains | Only after approval by a Ruum admin, per tenant. |
| 23 | Internet access | Yes, through an egress proxy that logs and limits. |
| 24 | AI gateway | Not in the first version. Needs its own design first. |
| 25 | Cold start | 1-3 s is acceptable on cheap plans. |
| 26 | Rights inside apps | App-level roles. Per-object rights can be added later without changes to app roles (section 9.3). |

### 17.5 Step 4: Repos and builds

| # | Question | Decision |
|---|---|---|
| 27 | Code hosting | Forgejo. |
| 28 | Plain Git for developers | Default: yes. |
| 29 | GitHub link | Default: later. |
| 30 | Previews | Default: a database branch. |
| 31 | Tests in builds | Run when the project has tests; a failure blocks "Publish", not the preview. |

### 17.6 Still open (later steps)

32. **Albums**: sharing an album gives view access to its photos? *Recommended: yes.*
33. **Desktop sync** in the first year, or web, API and WebDAV only?
34. **Shared mounts**: allow sharing a personal mount, or only organisation-owned mounts?
35. **Vaults in the browser**: native apps only at first, or a web client with the risk documented?
36. **Vault recovery**: the organisation recovery key, on or off by default?
37. **AI gateway**: models, billing and limits (needs a short design of its own).
38. **Who pays for the model in agent workspaces** (section 11.1). *Decided 2026-11-06: both; the
    person's own API key first, Ruum billing through the AI gateway later.*
39. **First harnesses.** *Decided 2026-11-06: the Claude Agent SDK inside Ruum with the ACP layer in
    place; every outside agent (ChatGPT, Claude.ai, Claude Code, Codex) over MCP with full app tools;
    then OpenAI Codex inside Ruum soon after, for non-technical ChatGPT users.*
40. **Default federation policy** (section 7.5). *Decided 2026-11-06: off for team tenants until an
    admin turns it on; personal accounts can turn it on themselves.*

## 18. How to build Ruum with a swarm of AI agents

Agents write code fast. They are weak at keeping a large system coherent over months. So people
own the parts that keep it coherent (the contracts, the tests that define "correct", and the
merges of risky changes) and agents do the rest in parallel.

### 18.1 Contracts first

Before agents build a step, write and review its **contracts**. They are the only shared ground
between parallel agents:

- the REST API as OpenAPI, and the MCP tool list;
- the SpiceDB schema (section 3.3) and the table of permissions per node type;
- SQL migrations for the tables of the step;
- event and webhook payloads (JSON Schema), the provider contract (section 7.1), the manifests
  (`app.yaml`, the database manifest) as JSON Schema;
- Rust traits at module borders (storage, blob store, runtime driver, authz).

A contract changes only in a **contract PR** that a person approves. CI detects breaking changes
(OpenAPI diff, schema diff) and blocks them in normal PRs. Clients and types are generated from the
contracts in CI, never written by hand.

### 18.2 Tests are the definition of done

- **Acceptance tests are written before the code**, by a different agent from the one that writes
  the code, and a person reviews them. The implementing agent may not change them; if it thinks a
  test is wrong, it stops and asks.
- **Conformance suites** run against every implementation of a trait (memory and Postgres storage,
  Garage and Hetzner blob stores, Docker and gVisor runtime drivers).
- **Security invariants as tests**: the tenant isolation suite (for every endpoint: tenant B's
  token never sees tenant A's data), the permission matrix, token scope checks. Add a **mutation
  check** like the one in the `cloud` repository: break each security rule on purpose in a copy,
  and a test must fail for each break.
- Property tests for the tricky logic (permissions with links, the journal, chunking, merges).

### 18.3 The task card

Work goes to agents as small task cards, each about one pull request of at most a few hundred lines:

```markdown
Goal:        Move a node to another folder (POST /v1/nodes/{id}/move).
Contracts:   openapi.yaml#/paths/~1v1~1nodes~1{id}~1move, journal event `node.moved`.
May change:  crates/ruum-core/src/nodes/move.rs, its tests, the endpoint in crates/ruum-api.
Must not:    migrations, other modules, the OpenAPI file.
Done when:   tests/acceptance/nodes_move.rs passes; tenant isolation and permission suites pass.
Out of scope: moving across tenants, moving into a vault.
```

The "May change" list stops two agents from editing the same files. Migrations go through a queue:
one migration task at a time.

### 18.4 Roles in the swarm

| Role | Done by | Job |
|---|---|---|
| Architect | agent panel; a person is optional (section 18.9) | design, contracts, approval of contract PRs and of security-sensitive merges |
| UX designer | agent, with a UX review panel; a person is optional (section 18.9) | user journeys, information architecture, the design system, wireframes and UI copy before the UI task cards; the "agent experience" of MCP tools, CLI output and error hints; approval of UI merges (section 18.4.1) |
| Planner | agent | breaks a step into task cards with dependencies (a task graph), in the issue tracker |
| Test writer | agent | writes acceptance tests from the contracts, before the code |
| Implementer | many agents in parallel | one task card each, in its own git worktree, until CI is green |
| Reviewer | agent with a fresh context (ideally another model) | reviews the diff against the card and the contracts; only reads |
| Security reviewer | two agents with different lenses; a person is optional (section 18.9) | extra review for changes in auth, crypto, tenancy and secrets |
| Integrator | agent | merges in order, fixes conflicts, runs the full suite and the local stack |

Rules for implementers: three failed attempts at the same problem → stop and escalate (section 18.9).
No new dependency without approval (`cargo-deny` allow-list, supply-chain risk). No production
credentials; agents work against the laptop stack and staging only.

#### 18.4.1 UX design

Agents build screens fast, but without a design each screen solves the same problem in its own way.
The UX designer works one step ahead of the UI implementers, as the architect does for the code:

- **Before the build** (in the Specify phase): the user journeys of the step (for step 1: first
  sign-in, upload, share with a colleague, find an old version, connect an agent), the information
  architecture (navigation, names of things), wireframes or a clickable prototype, and the
  **design system** (tokens for colour, type and spacing, and the components: file list, share
  dialog, version panel, empty states, errors). UX needs can drive contract changes (a field the
  screen needs) before the contracts freeze.
- **In the task card** of a UI task: the journey, the wireframe or prototype link, every state
  (loading, empty, error, no permission, phone width, dark mode) and the exact UI copy.
- **Review**: a UX review agent compares screenshots of each UI branch (Playwright, desktop and
  phone, light and dark) with the design and the card. The UX review panel (section 18.9), or a
  person when one is available, approves UI merges.
- **Checks in CI**: accessibility (axe, WCAG 2.2 AA) and visual snapshots of the main screens, so
  an agent's change cannot quietly break a screen.
- **Agent experience**: AI agents are users too. The designer also owns the names and descriptions
  of MCP tools, the CLI output, and the hints in error messages, and reads the agent benchmark
  results (section 18.7) like usability test results.
- **Users**: at the end of each step, a usability test with synthetic users (section 18.9), and
  with 3-5 real people from the target group when they are available. The findings become task
  cards.

### 18.5 CI gates

Nothing merges red. Every PR runs: `cargo fmt`, `clippy -D warnings`, unit and conformance tests,
`cargo-deny`, the migration check, the contract diff, the tenant isolation and permission suites,
and end-to-end tests against the Compose stack (section 12.2). Nightly: mutation checks, the full
end-to-end suite on staging, and a restore test of the backups.

### 18.6 Memory for the agents

- A short `CLAUDE.md` at the root and one per crate: conventions, commands, what not to do.
- **Decision records** (ADR files, one per decision, like section 17) so an agent never reopens a
  decided question.
- A `PITFALLS.md` that agents and reviewers add to after each mistake that cost time.
- This design document stays the source of truth. A PR that changes behaviour updates it.

### 18.7 Rhythm per step

1. **Specify** (person + agent): contracts, user journeys and wireframes (section 18.4.1),
   acceptance tests, task graph. About a quarter of the time of the step. Do not skip it; it is what makes parallel work possible.
2. **Build** (swarm): task cards in parallel, merged continuously.
3. **Harden**: a pass of reviewer and security agents over the whole step, load tests, a restore
   test.
4. **Dogfood**: use Ruum for its own work. From step 3 on, run an **agent benchmark**: a fixed list
   of tasks such as "build a to-do app with Postgres and sign-in", done by outside agents (Claude,
   ChatGPT) through the MCP server against staging. The success rate is a release gate, because
   "easy for AI agents" is the product.

### 18.9 Running without people

People are **optional** in this method. Each gate that a person would hold has an agent
replacement, so the work continues when nobody has time. A person who has time can still take any
role, or veto a decision later.

| Gate | Agent replacement |
|---|---|
| Architect decisions and contract changes | An **architect panel**: one agent proposes; two independent agents with fresh context (ideally another model) review it from different angles (consistency with the design, cost of later steps). Both must accept. The contract diff must show no breaking change, or the change is additive. The result is a decision record marked `provisional`. |
| Open product questions | The panel takes the recommended answer, records it as a `provisional` decision with its reasons, and goes on. |
| Security-sensitive merges | Two security reviewers with different lenses, the isolation and permission suites, and the mutation check in strict mode. Any `must_fix` blocks the merge. |
| UX design and approval of UI merges | A UX designer agent, and a **UX review panel**: a heuristic review (Nielsen's ten heuristics), an accessibility review (axe and keyboard), and a consistency review against the design system, all on screenshots at desktop and phone width, light and dark. |
| Real users | **Synthetic users**: persona agents (for example "freelance landscape architect, not technical", "small team admin", "developer with Claude Code") drive the real UI in a browser with tasks and think aloud. Measure task success, steps and errors. Outside AI agents do the agent benchmark over MCP. |
| Escalation after three failed attempts | A fresh senior agent with the whole context tries once. If it also fails, the task card is parked as `blocked`, with a written question, and the swarm goes on with other cards. |
| Release of a step | Automatic gates: all CI gates, the end-to-end suite, the synthetic user tests above their thresholds, and the agent benchmark above its threshold. |

What agents cannot replace well, and how the method handles it:

- **Synthetic users are not real users.** They find problems of clarity, discoverability and broken
  flows. They cannot show what real people need, want to pay for, or do in their own work. Their
  findings are labelled as synthetic, and the product decisions they lead to stay `provisional`
  until real users confirm them.
- **Agent panels share blind spots.** Use different models where possible, give each reviewer a
  different lens, and keep the automatic checks (suites, mutation check, contract diff) as the
  hard floor.

**Things that stay with a person**, because they are outward-facing, cost money or cannot be
undone: registering domains, creating cloud and Hetzner accounts, payments, pushing to a public or
shared remote, publishing public apps, sending email to real people, and any production
credentials. Agents prepare these steps and stop.

**The person's view.** After each run, the agents publish a short **digest**: what was built, the
decisions made (with links to the decision records), the risks, and the parked questions. A person
can read it in a few minutes, and veto or change any `provisional` decision. Without a veto, the
decisions stand.

**Guardrails for unattended runs**: a token and time budget per run, the disk check before builds,
no network actions outside the laptop stack and package registries, and a stop when a guardrail is
hit, with a report.

### 18.8 The first tasks

1. The repository: Cargo workspace, web UI project, `scripts/dev.sh`, CI with all gates (empty
   suites at first).
2. The local stack of section 12.2 and the seed script.
3. The step 1 contracts: OpenAPI, SpiceDB schema, the first migrations, the journal format.
4. The tenant isolation and permission test harness, before the first endpoint.
5. Then the parallel work: nodes, blobs and versions, sharing, agents and tokens, the MCP server,
   WebDAV, the web UI.

## 19. Operating Ruum with agents

Ruum must not need a large operations team. A swarm of agents runs it, a policy keeps them safe, and
people decide only what needs human judgment. Section 18 is how agents build Ruum; this section is how
they run it.

### 19.1 Three layers

1. **A self-healing platform, with no agent in the loop.** Routine work is deterministic: Kubernetes
   controllers, database failover (CloudNativePG), autoscaling, certificate renewal, automatic
   rollback on a rising error rate (9.6), scheduled backups with automatic restore tests, dependency
   update bots behind the CI gates. Ruum stays up when every agent, or the AI provider, is down.
2. **An agent swarm for the exceptions** (19.2).
3. **People for decisions that need human judgment** (19.4).

### 19.2 Agent roles

| Role | Job |
|---|---|
| On-call | receives every alert, at any hour; correlates and diagnoses with logs, metrics and traces; runs runbook actions within policy; escalates |
| Remediation | turns a diagnosis into a fix through the normal pipeline (branch, CI, review panel, staged rollout) |
| Release | promotes builds from staging to a canary cell and then to all cells, after automatic canary analysis |
| Security | vulnerability and image scans, patches, secret rotation, audit-log anomalies, abuse and phishing takedowns for public apps (13) |
| Capacity and cost | forecasts, scaling, rightsizing, new cells (14) |
| Support | answers customer tickets with the documentation and read-only diagnostics, only with the customer's consent |
| Compliance | access reviews, proof of backups and restores, GDPR export and erasure requests, audit evidence |
| Postmortem | writes incident reports and turns lessons into task cards |
| Guard | a second, independent agent (another model where possible) that must agree before amber actions |

### 19.3 Safety: the policy decides, not the agent

All operations go through **one audited operations API with a policy engine**. Agents get no shell and
no standing administrator rights. Each action has a risk class:

| Class | Who decides | Examples |
|---|---|---|
| Green | the agent, within limits | restart, scale within budget, roll back, renew a certificate, retry a backup |
| Amber | two independent agents (the guard included); logged | database failover, moving a tenant between cells, blocking an abusive app, revoking tokens, a production security patch |
| Red | a person | deleting tenant data, a restore that loses data, changing security policy or access rights, spending above budget, legal and law-enforcement requests, telling customers about a breach, turning off a safety control |

- **The policy is signed and owned by a person**; agents cannot change it.
- **Just-in-time credentials**: each action gets short-lived, narrow credentials from OpenBao.
  Production data reaches agents only through the privacy filter (8.5, 9.6), as pseudonyms.
- **GitOps**: every infrastructure change is a pull request, reviewed by an agent panel and checked by
  the policy, rolled out staging → canary cell → all cells. Cells limit the blast radius (14).
- **Prompt injection**: alerts, logs, tickets and customer messages are untrusted data. Operations
  agents never act on instructions inside them, and their tools are an allow-list.
- **Limits and kill switches**: rate limits and token budgets for the operations agents; one switch
  for a person to stop the whole swarm (layer 1 keeps running).
- **No single AI provider**: a fallback model or provider for the operations agents.

### 19.4 The person's part

- **Red actions** arrive as a push notification with a short summary, the recommended choice and its
  consequences: approve or reject.
- **Any security incident with possible data loss pages a person.**
- **A weekly operations digest**: incidents, SLOs, costs, security, and the agent decisions that a
  person can veto.
- **A monthly review** of the policy, the SLOs and the budgets (about 15 minutes).

### 19.5 Proof before production

- **Game days on staging**: injected failures (lost nodes, a corrupt backup, a simulated breach, an
  abuse flood) that the agents must handle within policy.
- **An operations benchmark**: a fixed set of incident scenarios; its pass rate is a release gate for
  running production.

This work is part of the staging step (section 16): there is nothing to operate before staging exists.
