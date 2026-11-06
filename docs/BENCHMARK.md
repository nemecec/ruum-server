# Agent benchmark

This page explains the agent benchmark of step 1 and gives the results of the first run. The
benchmark is the release gate of design section 18.7 ("easy for AI agents" is the product), and the
"outside AI agents" replacement of section 18.9. The harness is in `bench/agent/`.

## What it measures

An outside agent gets a task in a user's words and works only through the Ruum MCP server
(`/mcp`). A checker then reads the final state through the REST API. The checker never reads what
the agent said. A task passes only when every check passes.

For each task the runner records the result of each check, the number of turns and tool calls,
the tool errors (code and message), the wall time, the cost and the agent's final answer. The
full transcript of each task is kept too.

## How it works

1. The runner starts `ruum-server` on port 8494 against the laptop stack (Postgres, SpiceDB,
   Garage), with fresh signing keys.
2. For each task it runs `ruum-server seed-dev`, so each task gets a **new tenant**: Alice (the
   owner), Bob (a guest without grants) and Carol (another tenant). It then waits 6 s, because the
   seed writes its SpiceDB tuples from another process (see `docs/STATUS.md`, known gaps).
3. It runs the task's `setup` steps through the REST API (folders, files, versions, grants, other
   agents and what they do).
4. It registers a **Benchmark agent** for the task's persona (Alice or Bob) with
   `registerAgent` and `createAgentKey`, and exchanges the key for a 15-minute access token.
5. It runs the outside agent: the Claude Code CLI in headless mode, in an empty temporary folder:

   ```sh
   claude -p "<task prompt>" --model sonnet --max-turns 25 \
     --mcp-config <file: {"mcpServers":{"ruum":{"type":"http","url":"http://127.0.0.1:8494/mcp",
                          "headers":{"Authorization":"Bearer <agent access token>"}}}}> \
     --strict-mcp-config --allowedTools "mcp__ruum__*" --tools "" --setting-sources "" \
     --no-session-persistence --append-system-prompt "<frame>" \
     --output-format stream-json --verbose
   ```

   `--tools ""` removes the built-in tools, so the agent has the 48 Ruum tools only.
   `stream-json` is the event stream of `--output-format json`; its last event is the same
   result object, and the events before it show each tool call and each tool error. The frame
   tells the agent that the user cannot answer questions, and that it must say in its answer what
   it did not do (`bench/agent/lib/agent.mjs`, `APPEND_SYSTEM_PROMPT`). The MCP config file holds
   the token; the runner deletes it after the run.
6. It runs the checker as Alice and writes the results.

### Run it

```sh
scripts/dev.sh status                      # the stack must be healthy
cargo build -p ruum-server                 # with the cargo settings of docs/STATUS.md
cd bench/agent && npm ci
node run.mjs                               # all tasks, the outside agent
node run.mjs --only tag_pdfs,share_view    # some tasks
node run.mjs --mode oracle                 # the reference solutions instead of the agent
node run.mjs --mode null                   # no action at all
node run.mjs --mode smoke --only share_view   # one get_me call: tests the MCP wiring
```

Options: `--port` (8494), `--model` (sonnet), `--max-turns` (25), `--server-bin`
(`target/debug/ruum-server`), `--run` (the run name), `--timeout-min` (14, under the token life).
A worktree has no `deploy/dev/.env`, so the runner reads `scripts/dev.sh env` of the main checkout.
The results are in `bench/agent/results/<run>.json`, and the transcripts in
`bench/agent/results/<run>/<task>.jsonl`.

### The checkers are tested

Each task has a `reference` solution in REST steps. `--mode oracle` runs it instead of the agent,
and every task must pass. `--mode null` does nothing, and every task must fail except the refusal
task, where doing nothing is right. Both modes ran on 2026-11-06: oracle 8 of 8 passed, null 1 of 8
passed (`readonly_key`), as expected. The reports are `results/2026-11-06-oracle.json` and
`results/2026-11-06-null.json`.

## Task files

A task is a YAML file in `bench/agent/tasks/`:

| Key | What |
|---|---|
| `id`, `title` | name of the task |
| `kind` | `normal`, `trap` (a pitfall the agent must avoid) or `refusal` (the right outcome is no change) |
| `persona` | the user the benchmark agent acts for: `alice` (owner) or `bob` (guest) |
| `agent_scope` | optional scope of the benchmark agent (default: whole tenant, `max_role: manage`) |
| `prompt` | the user's words; `${BOB_EMAIL}` and the other seed values are filled in |
| `setup` | REST steps before the agent runs |
| `check` | assertions on the final state, each with a `what` line |
| `reference` | a correct solution in REST steps, for `--mode oracle` |
| `answer_should_match` | optional regex on the final answer; reported as `answer_matches`, **not** part of success |

Setup and reference steps (`bench/agent/lib/steps.mjs`): `folder`, `file` (with `content` or
`base64`, `type`), `write`, `label`, `agent` (registers an agent; later steps act as it with
`as: <name>`), `grant`, `tag`, `attach`, `rename`, `move`, `trash`, `restore`,
`restore_version`, `sleep`. A step acts as `as` (default `alice`); paths are resolved by Alice, so
a step can name a node that its principal reaches only by ID. `save: <name>` keeps a node ID for
the checks.

Checks (`bench/agent/lib/checks.mjs`): `live`, `gone`, `path` (`equals` or `parent`), `exists`,
`absent`, `content` (`equals`, `contains`, `not_contains`), `children` (`names`, `count`,
`at_least`), `grant`, `direct_grants`, `share_links`, `agents`, `tags_named`, `tag_nodes`,
`search_count`, `versions`. A saved node is checked by ID, so a copy-and-trash answer to "move"
fails.

Quote a `what` text that has a comma in a flow map (`{ what: "a, b", ... }`); the loader refuses a
key without a value.

## The step 1 tasks

| Id | Kind | Persona | Task | Main checks |
|---|---|---|---|---|
| `organise_by_year` | normal | Alice | Sort 12 files in Inbox into folders 2022, 2023, 2024. The year is written in four ways. | Inbox holds only the three folders; each original node is in its year folder |
| `restore_before_budget` | normal | Alice | Find the version of `Projects/plan.txt` from before the budget change and restore it. Four versions, alternately by Alice and a helper agent. | head content is version 2; history kept (5 versions) |
| `share_view` | normal | Alice | Share Projects with `mari@example.test`, view only. | one direct `view` email grant; no share link; Private not shared |
| `readonly_key` | refusal | Alice | Create a read-only API key for a backup script, limited to Reports. Keys are user-only, so the agent cannot. | no new agent; no share link or grant on Reports (no workaround) |
| `tag_pdfs` | normal | Alice | Tag every PDF in Docs (with subfolders) as `invoice`. Traps: `.PDF`, `pdf-tips.txt`, a trashed PDF, a PDF outside Docs. | one `invoice` tag, on exactly the four live PDFs in Docs |
| `undo_agent` | normal | Alice | Undo everything the agent "Sorter bot" did today (rename, move, edit, new folder and file, trash), and keep Alice's own changes. `undoAgent` is user-only, so the agent undoes by hand. | each bot change reverted; Alice's edit and new file kept |
| `undo_agent_two_bots` | normal | Alice | Two agents of Alice worked in Notes; undo only what "Sorter bot" did, keep the work of "Tidy bot" and of Alice (decision 0003-5: the agent tells them apart by `actor.display_name` or the `actor` filter). Added 2026-11-06; not run yet. | Sorter bot's rename, move and edit reverted; Tidy bot's move and new file kept; Alice's new file kept |
| `name_clash` | trap | Alice | Move `Drafts/report.txt` into Final, which already has a `report.txt`. | the draft is in Final under any name; the old Final file is live and unchanged |
| `cannot_edit` | trap | Bob | Fix a typo in `Team/notes.txt` (Bob may edit) and in `rules.txt` (shared with Bob for viewing only). | notes fixed; rules unchanged; no copy of rules made |

## Results of the first run (2026-11-06)

Run `2026-11-06-sonnet`: `claude-sonnet-5-5` (alias `sonnet`), Claude Code 2.1.319, 25 turns at
most, `main` at `c89bff7`. Report: `bench/agent/results/2026-11-06-sonnet.json`.

**7 of 8 passed (88 %).** 55 tool calls, 1 tool error, 100 s of agent time, USD 0.39 in all.

| Task | Kind | Success | Turns | Tool calls | Tool errors | Agent time (s) | Cost (USD) |
|---|---|---|---|---|---|---|---|
| organise_by_year | normal | yes | 18 | 17 | 0 | 18 | 0.11 |
| restore_before_budget | normal | yes | 8 | 7 | 0 | 16 | 0.04 |
| share_view | normal | yes | 3 | 2 | 0 | 8 | 0.02 |
| readonly_key | refusal | yes | 1 | 0 | 0 | 10 | 0.02 |
| tag_pdfs | normal | yes | 13 | 12 | 0 | 14 | 0.06 |
| undo_agent | normal | yes | 10 | 9 | 0 | 13 | 0.08 |
| name_clash | trap | no | 4 | 3 | 1 | 10 | 0.02 |
| cannot_edit | trap | yes | 6 | 5 | 0 | 10 | 0.03 |

`answer_matches` (not part of success): `readonly_key` yes (it said it cannot create keys and did
not use a workaround), `cannot_edit` yes (it said that `rules.txt` is view-only).

This is one run of one model. With 8 tasks, one task is 12.5 points, so the rate is noisy. Run
the benchmark several times, and with other agents, before it becomes a gate with a threshold.

## Failure analysis

### `name_clash` failed: from the agent (a safe failure)

The agent called `move_node` without `on_conflict` and got:

```
name_taken: the folder already has a node named "report.txt"
hint: pick another name, or send on_conflict: rename
```

The error and the hint were right and named the fix. The agent then chose not to move the file,
because it "can't tell which version you want to keep", and asked the user to choose. It lost no
data. The frame told it to do the task as well as it can. So the cause is the agent's caution, not
Ruum. Ruum can still help: if `move_node` said up front that a clash fails and that
`on_conflict: rename` keeps both files, the agent could plan for it (fix 5 below).

The checker counts a move as success. "Stop and ask" is a fair answer from a careful agent, too.
Keep the check as it is (the user asked for a move, and the hint gave a safe way to do it), but
decide this before the benchmark becomes a gate: one option is a third outcome, "safe no-op", that
counts half.

### Passes with friction: from Ruum

The transcripts show where Ruum made the agent work harder or guess. These did not fail a task,
but they will fail harder tasks.

1. **The journal does not say who an agent is.** In `undo_agent` the entries of `list_changes`
   have `actor: {"id": ..., "kind": "agent"}` without `display_name` (the contract says "filled
   in responses when known"; the core never fills it). Agents cannot call `listAgents` or
   `getAgent` (`not_exposed`). The agent found "Sorter bot" only because one other agent wrote in
   the tenant. With two agents it would have to guess, and it could undo the wrong agent's work.
   The same gap is in `created_by` and `updated_by` of nodes and in `list_versions`.
2. **No way to filter the journal by actor or time.** `list_changes` has `cursor` and `node` only.
   The agent read the whole tenant journal (`limit: 1000`) and filtered it by itself. That works for
   a fresh tenant, not for a real one. `listAgentActivity` has `since` but is not exposed.
3. **Undo is user-only, and nothing says so.** The agent rebuilt `undoAgent` by hand (rename, move,
   restore version, restore, trash: 9 calls). It worked, but the server already knows the inverse of
   each event. The tool list does not say that undo exists for the user.
4. **No pointer to the place where the user does user-only work.** In `readonly_key` the agent
   correctly refused, but it had to guess where the user makes a key ("Ruum's own settings or admin
   UI, or with the `ruum` CLI if it supports that"). Nothing in `get_me` or the server instructions
   says where agents and keys are managed.
5. **Name clashes are found by trial.** `move_node`, `copy_node`, `rename_node` and `create_node`
   do not say in their descriptions that a taken name fails with `name_taken`, or that
   `on_conflict: rename` exists.
6. **Finding files by type takes a tree walk.** In `tag_pdfs` the agent listed five folders one by
   one (`list_children`), because `search_nodes` needs a name text (`q`, at least 1 character) and
   has no MIME type filter. It also cannot list a subtree in one call.
7. **One call per item.** `organise_by_year` needed 12 `move_node` calls and `tag_pdfs` 4
   `attach_tag` calls. Each is a turn of the agent. At 25 turns, a folder of 30 files cannot be
   sorted.

What worked well, and should stay: `my_role` and `my_can_share` on every node (in `cannot_edit` the
agent saw `my_role: view` on `rules.txt` from `list_shared_with_me` and did not try to write it,
so there was no `permission_denied` at all); the `name_taken` hint; `list_versions` plus
`read_file` with `version` (the agent found the budget change in 7 calls); email grants in
`add_grant`; `restore_version` that keeps the history.

### Concrete fixes for the MCP tool list and errors

Changes to `contracts/mcp/tools.yaml` (descriptions only, no contract change):

| Tool | New or changed description |
|---|---|
| `move_node` | "Move a file or folder to another folder (needs manage on it and edit on the target). If the target folder has a node with the same name, the call fails with `name_taken`; send `on_conflict: rename` to keep both (adds \" (2)\")." |
| `copy_node`, `rename_node`, `create_node` | add the same sentence about `name_taken` and `on_conflict: rename`. |
| `list_changes` | add: "Each entry has `actor` (kind, id, display_name) and `on_behalf_of`. Use `get_me` to know your own ID." (after fix 1 of the next table) |
| `search_nodes` | add: "`q` matches names only; to find files by type use `type` (after it exists) or walk the folders with list_children." |
| `get_me` | add: "Agents cannot create agents, API keys, app passwords or invitations, and cannot undo another agent; the user does that in the web UI under Agents." |

Server instructions (`crates/ruum-mcp/src/server.rs`, `INSTRUCTIONS`): add one sentence: "Some
actions are for the signed-in user only (agents, API keys, app passwords, members, invitations,
undo of an agent, audit); tell the user to do them at `<public URL>/agents` (or the right page)."

Product and contract changes (each is a separate task card):

| # | Fix | Why (task) |
|---|---|---|
| 1 | Fill `display_name` of `PrincipalRef` in journal entries, versions and `created_by`/`updated_by` (agent name, user name). **Done** (decision 0003-5): every answer names its actors, agents also their user (`on_behalf_of_display_name`). | undo_agent: find "Sorter bot" by name |
| 2 | Add `actor` (principal ID) and `since` (timestamp) filters to `listChanges`, or expose `listAgentActivity` as a tool for agents of the same user (read-only). **Done** (decision 0003-5): the `actor` and `since` filters; `listAgentActivity` stays hidden from agents (0002-5 row 54). | undo_agent: read only the bot's entries |
| 3 | Expose `undoAgent` with `dry_run: true` only, or a new tool `preview_undo_agent`, so an agent can show the user what an undo would do; the real undo stays user-only. | undo_agent: 9 hand-made calls |
| 4 | A `type` (MIME, with prefix match such as `image/`) filter on `searchNodes`, and `q` optional when `within`, `tag` or `type` is given. | tag_pdfs: tree walk of 5 calls |
| 5 | A batch tool (for example `move_nodes`, `attach_tag` with many node IDs) or a JSON batch endpoint, each item with its own result. | organise_by_year: 12 calls |
| 6 | A `user_required` error hint that names the page of the web UI (for example "ask the user to create the key at https://<host>/agents"), from `RUUM_PUBLIC_URL`. | readonly_key: the agent guessed the place |

## Notes

- The benchmark found no tool error caused by an unclear input schema: every tool call had valid
  arguments the first time.
- `share_view` sends an invitation email to `mari@example.test`; on the laptop it goes to Mailpit.
- The benchmark runs on the laptop stack, not on staging (design 18.7 says staging, from step 3).
- The run used the copy of `target/debug/ruum-server` built from `c89bff7`, so builds by other
  worktrees in the shared cargo cache could not change the binary during the run.
