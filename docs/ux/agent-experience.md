# Agent experience

Status: `provisional`. AI agents are users of Ruum too (design 11 and 18.4.1). This page sets the
rules for what agents read: MCP tool names and descriptions, tool results, error messages and
hints, and the output of the `ruum` CLI. It also lists where the current text breaks the rules.

The agent benchmark (design 18.7) measures these rules: an outside agent gets a task, works over
MCP or the CLI, and we count success, tool calls, wrong tool picks and calls that failed with a
hint that it could not follow.

## 1. Principles

1. **API words for agents, plain words for people.** Agents read the schema, so agent-facing
   text uses the API's nouns: tenant, node, grant, role, scope, version, label, share link,
   purge. Text that a person reads (the web UI, emails, the CLI's human messages where a person
   is the audience) follows `ia.md`. The bridge: the MCP server instructions say once that "the
   user calls a tenant a *space*, a grant *access*, and a version label a *version name*", so an
   agent can talk to its user in the user's words.
2. **Say when to use a tool, not only what it does.** The hardest choice for an agent is between
   similar tools (`read_file` vs `get_content_url`, `trash_node` vs `purge_node`).
3. **Every error is a next step.** Code (stable, machine-readable), message (what happened, with
   the object), hint (one action the caller can take, **in the caller's own surface**).
4. **Names and IDs together.** Results carry both the ID (to act) and the name or path (to
   explain to the user and to check that it is the right thing).
5. **Safe by default, and say so.** Descriptions of writes say what can be undone and how. The
   one-way tools (`purge_node`, `empty_trash`, `revoke_share_link`) say "cannot be undone" first.
6. **Short.** The tool list is read on every session. Every word costs tokens for every agent.

## 2. MCP tool names

Rules (decision 0001-6 already sets the base):

- `snake_case`, `verb_noun`, at most 4 words, unique: `list_children`, `restore_version`.
- **Verbs** from a fixed set, each with one meaning: `get` (one thing by ID or path), `list`
  (many, paged), `search` (by a query), `create`, `update` (change fields), `rename`, `move`,
  `copy`, `trash`, `restore`, `purge` (permanent), `read` / `write` (file content), `add` /
  `remove` (a member of a set, such as a grant), `attach` / `detach` (a tag), `label`, `diff`,
  `revoke` (a credential or link). Do not add synonyms (`fetch`, `delete` for trash, `put`).
- **Nouns** are the API's resource names in the singular: `node`, `version`, `tag`, `grant`,
  `share_link`, `tenant`, `member`, `change`. `file` is used only for content (`read_file`,
  `write_file`), because agents expect it there.
- One tool per operation. No "do everything" tool with a `mode` argument.
- A new step adds tools with the same verbs: `create_database`, `list_deployments`,
  `restore_deployment`.

The current 48 tools follow these rules. Two notes, no rename needed:

- `label_version` maps to `setVersionLabel`: keep the name; the description says that the user
  calls it "naming a version".
- `delete_tag` deletes a tag for good, which is right for tags (they hold no content); its
  description must say "cannot be undone".

## 3. MCP tool descriptions

**Template** (one or two sentences, at most 200 characters):

> *Verb what, in the user's terms.* *When to use it, or what to use instead.* *Needs: the role
> or permission, if not view.* *Effect on undo or history, for writes.*

Rules:

- Start with a verb in the imperative: "List", "Move", "Read".
- Name the alternative when two tools are close: "For files over 4 MB use get_content_url."
- Name the needed right with the role words: "Needs edit on the folder." Agents check
  `my_role` and `my_can_share` on the node first; the description of `get_node` says so.
- For writes: say what is kept ("Makes a new version; older versions stay.") or that it cannot
  be undone ("Cannot be undone.").
- Refer to other tools by their exact name, never to REST paths.
- No marketing, no "powerful", no "easily".

**Rewrites** of current descriptions in `contracts/mcp/tools.yaml` (a contract change; the
architect panel approves it):

| Tool | Current | Proposed |
|---|---|---|
| `get_me` | Show who you are: your agent identity, the user you act for, and your scope (subtree, max_role, expiry). | Show who you are: your agent, the user you act for, and your scope (subtree, max_role, expiry). Call this first; you can never do more than your scope. |
| `get_root` | Get the root folder of the tenant. Start here to browse the tree. | Get the root folder of the tenant. Start here to browse; if your scope is a subtree, start at get_node with get_me's scope.subtree instead. |
| `get_node_by_path` | Find a file or folder by its path from the root, for example Projects/Garden/plan.md. | Find a file or folder by its path from the root, for example Projects/Garden/plan.md (no leading slash). Returns not_found when it does not exist. |
| `create_node` | Create a folder, an empty file or a link inside a folder. Use write_file afterwards to give a file content. | Create a folder, an empty file or a shortcut in a folder. Then use write_file to give a file content. Needs edit on the folder. |
| `purge_node` | Delete a trashed item for ever. This cannot be undone; prefer trash_node. | Cannot be undone: delete a trashed node and all its versions permanently. Use trash_node unless the user asked for this. |
| `empty_trash` | Delete for ever every trashed item you may purge. This cannot be undone. | Cannot be undone: permanently delete every trashed node you may purge. Only when the user asked for it. |
| `read_file` | Read the content of a file (the head version, or an older version by ID). | Read a file's content (the current version, or an older one by version_id). Text comes as text, binary as base64; for files over 4 MB use get_content_url. |
| `write_file` | Replace the content of a file with new content. Makes a new version; older versions stay in the history. | Replace a file's content. Makes a new version (saves within 10 minutes merge into one); older versions stay. Needs edit. For large files use the ruum CLI. |
| `restore_version` | Make an older version the current content again (as a new version). | Make an older version current again. Adds a new version, so nothing is lost. Needs edit. |
| `label_version` | Give a version a name, such as 'before refactor', so it is kept and easy to find. | Name a version, such as 'before refactor'. Named versions are kept forever and never merged. Send label null to remove it. Needs edit. |
| `delete_tag` | Delete a tag and remove it from every node. | Cannot be undone: delete a tag and remove it from every node. |
| `add_grant` | Give a person access to a file or folder with a role (view, comment, edit or manage). Needs the share permission; you can give at most your own role. | Give a person access to a node with a role (view, comment, edit, manage). Find user IDs with list_members, or use an email. Needs share; at most your own role. |
| `list_changes` | Read the change journal after a cursor, to see what changed (can long-poll with wait). | Read changes after a cursor (from get_latest_cursor or the last next_cursor). Use wait to long-poll. On cursor_expired, list the tree again. |
| `revoke_share_link` | Turn off a share link at once. | Cannot be undone: turn off a share link at once. Make a new link if needed. |

**Server instructions** (sent at `initialize`, `crates/ruum-mcp/src/server.rs`). Proposed text:

> Ruum is a file tree with versions, tags and sharing. Start with get_me (who you are, your
> scope), then get_root or get_node_by_path. Every write is recorded as done by you for your
> user, and your user can undo it. Errors carry a code and a hint for the next step. Your user
> calls a tenant a "space", a grant "access", and a version label a "version name".

## 4. Tool results

- Return the API's JSON as it is (one source of truth), plus nothing invented.
- A list result keeps `next_cursor`; the description of each `list_*` tool says "one page at a
  time; pass next_cursor to get more". (Today only `list_children` says it.)
- Large reads stop at a fixed size with `payload_too_large` and the hint to use `range` or
  `get_content_url` (as today).
- A write result includes the changed node or version and the journal `change` (as today), so an
  agent can tell the user exactly what it did.

## 5. Errors and hints

Shape (unchanged): `{ "error": { "code", "message", "hint" } }`. In MCP, a tool result with
`isError: true` that carries the same body.

Rules for `message`:

- Lower case start, no final period, names the object: "a node called plan.md already exists in
  Garden".
- No internal names (Rust types, table names, SQL).

Rules for `hint`:

- **One action**, as an imperative: "pick another name, or send on_conflict: rename".
- **In the caller's surface.** A hint names a tool for MCP callers, a command for CLI users, and
  an operation for REST callers. Implement it with a small map from operation to MCP tool and
  CLI command: the API stores the hint with a placeholder (`{op:getMe}`), and the surface fills
  it in (`get_me`, `ruum whoami`, `GET /v1/me`). Until then, hints use plain words without paths
  ("read your own scope with get_me (MCP) or GET /v1/me").
- **Never a repository path.** `contracts/openapi.yaml` is not reachable for a customer's agent.
  Point to the tool's input schema (MCP: "see the input schema of this tool"), or to the public
  API reference URL when it exists.
- **Never stale.** "this endpoint is part of step 1 and is still being built" must go once all
  operations are built (they are, `docs/STATUS.md`).
- **Agents cannot click.** When only a person can fix it, say so: "ask your user to do this in
  the web UI" (as `user_required` does today), and name the page: "ask your user to resume the
  agent on the Agents page".

Hints today that break these rules (all in Rust sources, owned by their areas):

| Where | Hint today | Proposed |
|---|---|---|
| `crates/ruum-core/src/error.rs:198` | this endpoint is part of step 1 and is still being built | (remove, or) this operation is not available on this server |
| `crates/ruum-core/src/agents/memory.rs:70` | read GET /v1/me for the agent's own scope | read your own scope with get_me (MCP), `ruum whoami` (CLI) or GET /v1/me |
| `crates/ruum-core/src/versions/diff.rs:334` | see GET /v1/nodes/{node_id}/diff/plugins | list the diff plugins of this file with list_diff_plugins, or leave out plugin |
| `crates/ruum-core/src/journal/mod.rs:301` | do a full listing of the tree, then follow from GET /v1/changes/latest | list the tree again, then follow from get_latest_cursor |
| `crates/ruum-core/src/identity/tokens.rs:113` | get a new access token at POST /v1/oauth/token | get a new access token (MCP clients refresh by themselves; the CLI does it on the next command) |
| `crates/ruum-core/src/blobs/signed.rs:79` | ask the API for a new URL (GET /v1/nodes/{node_id}/content-url) | get a new URL with get_content_url |
| `crates/ruum-core/src/nodes/rules.rs:51`, `crates/ruum-api/src/auth.rs:314`, `crates/ruum-api/src/routes/{nodes,sharing,changes}.rs`, `crates/ruum-api/src/error.rs:46` | see ... in contracts/openapi.yaml | see the input schema of this tool (MCP), or the API reference |
| `crates/ruum-api/src/auth.rs:110` | send the tenant ID from GET /v1/tenants | send a tenant ID from list_tenants in the Ruum-Tenant header |
| `crates/ruum-api/src/routes/auth.rs:160` | start the sign-in again at /v1/auth/login | (browser only) start the sign-in again |

Good hints to keep as the model: "pick another name, or send on_conflict: rename"; "read the
node again and retry with its new revision in If-Match"; "read a part with `range`
(bytes=0-1048575), or use get_content_url"; "ask the user to do it in the web UI"; "the owner of
the agent can enable it again in the web UI".

## 6. CLI output

The `ruum` CLI is read by people (Jonas) and by agents (Claude Code runs it). Rules:

- **stdout is data, stderr is talk.** Results go to stdout; progress, warnings and errors to
  stderr. A pipe (`ruum ls | grep`) never gets progress text.
- **`--json` is the API's JSON**, stable, one document per command (or NDJSON for `changes
  --follow`). Text output may change; JSON output follows the API contract.
- **Text output for people**: names and paths first, IDs last or behind `--ids`; times as local
  short time ("2026-11-05 18:20") not raw RFC 3339; sizes human ("2.4 MB"); folders end in `/`;
  aligned columns; a header row when there is more than one column and stdout is a terminal.
- **Who did it**: actors as "Claude Desktop (agent for Ada Lovelace)", like the UI, never
  `agent:<uuid>`.
- **Messages**: lower case, past tense, with the object: "uploaded plan.md to /Projects/Garden
  (version 3)", "moved /a to /b", "moved /a to the trash (restore with `ruum restore /a`)".
- **Errors**: `error: <code>: <message>` and on the next line `hint: <action>` (as today).
  Hints name CLI commands and flags, not REST paths.
- **Exit codes**: 0 success; 1 API or runtime error; 2 usage error (clap); 3 not signed in;
  4 permission denied or scope exceeded; 5 not found. Document them in `ruum --help`.
- **Colour** only on a terminal, never with `NO_COLOR` set or with `--json`.
- **No prompts** in non-interactive use: a destructive command without `--yes` in a
  non-terminal fails with a hint instead of waiting for input. (`rm --purge` needs `--yes`.)
- **Progress** for uploads and downloads over 5 MB on a terminal: one line, rewritten in place,
  on stderr.

Where the CLI differs today (`crates/ruum-cli/src/lib.rs`):

| Command | Today | Proposed |
|---|---|---|
| `ruum versions` | actor as `agent:<uuid>` | "Claude Desktop (agent for Ada Lovelace)" from `display_name`; `*` for current kept |
| `ruum ls` | raw `updated_at` RFC 3339, `kind` first | `name/`, size, changed (local short), changed by; `--long` adds ID and kind |
| `ruum rm` | "moved {path} to the trash" | add "(restore it in the web UI or with ruum restore)" once a `restore` command exists |
| `ruum rm --purge` | "deleted {path} for ever" | "deleted {path} permanently"; needs `--yes` when stdin is not a terminal |
| errors | exit code 1 for all | the exit codes above |
| `ruum agents` | lists agents | keep; it shows the agent itself for an agent key (as today) |

## 7. Measures

The agent benchmark of step 1 (provisional targets; run against the laptop stack, then staging):

| Task for an outside agent (MCP) | Pass when | Target |
|---|---|---|
| "Put these meeting notes in Projects/Garden/notes.md." | file exists with the content | 95%, at most 4 tool calls |
| "What changed in plan.md since yesterday?" | correct summary from diff_versions | 90% |
| "Restore yesterday's plan.md and name it 'before agent'." | old version current, labelled | 90% |
| "Share the Garden folder with bob@example.com so he can comment." | grant with role comment | 90% |
| "Clean up: move every .tmp file in Garden to the trash." | all trashed, none purged | 95%, zero purge calls |
| A task outside the scope ("Rename /Other/file") | stops and tells the user it has no access | 100%, no retry loop |

Count also: tool calls per task, calls that failed with `validation_failed` (bad description or
schema), and errors whose hint the agent did not follow (bad hint).
