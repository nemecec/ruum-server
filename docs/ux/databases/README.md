# UX design of step 2: databases

Status: `provisional` (design section 18.9), 2026-11-06. Written by the UX designer agent before
the build (design 18.4.1, card U-1 of `docs/STEP2.md`), against the revised contracts of
`f/step2spec` (decision record 0004). A person can veto or change any part. No real user has
seen these screens; the synthetic users of step 2 test them after card U-2.

| Page | What it holds |
|---|---|
| [journeys.md](journeys.md) | The eight step 2 journeys (J7 to J14): steps, screens, success criteria, states. |
| [screens.md](screens.md) | Wireframes of every database screen, who sees what, and the states of each screen. |
| [../design-system.md](../design-system.md) | New components 4.19 to 4.26 (database item and status, query editor, result grid with hidden-value chips, classification review list, write-grant prompt, request card, restore picker, query log). |
| [../copy.md](../copy.md) | Section 3.8 (databases), the new error codes in section 4, new empty states in section 5. |
| [../agent-experience.md](../agent-experience.md) | Section 8: rules for the 18 database MCP tools, the server instructions, hints and the `ruum db` CLI. |
| [../ia.md](../ia.md) | Names of the step 2 concepts and the database URLs. |

## Who the journeys are for

The personas of `../personas.md`, in step 2 situations:

| Persona | Step 2 situation |
|---|---|
| Jonas (developer, Claude Code) | Builds a small garden planner app in his personal space. Claude Code makes the database, writes migrations on a branch, and asks for write access when it must fix live data. Jonas reads what Claude ran. |
| Marko (studio admin) | Makes a project tracker database in a team folder that guests can see. Wants to know who can read it, and wants a way back when something goes wrong. |
| Liis (not technical) | Her Claude keeps her plant list as a database. She never writes SQL. She must confirm privacy classes and approve or deny requests in plain words, and must never be scared into a wrong click. |

Helena (guest) appears only as a person who can see a project folder: she can read a database
in it, but cannot make one (`guests_create_resources` is false by default).

## Design principles for databases

1. **The safe choice is the default everywhere.** Read only, masked copy, restore as a new
   database, write access for one hour, Personal for an unknown column.
2. **Say what an agent sees.** Every screen that shows data or gives access says in one line what
   an agent will see or may do ("Agents see a stand-in, the same for the same value").
3. **Stand-ins and hidden values never look like data.** A chip with a dashed border and an icon,
   never a string that could pass as a name or a password (design 8.5, "typed markers").
4. **Every way forward has a way back.** Write access, migrations and in-place restores all
   record a restore point first; the screens say so and link to it.
5. **People decide; agents ask.** The UI shows a request with the statement in words, the number
   of rows and who asked, and asks for one click. It never decides for the person, and a link from
   an agent only opens a prompt; it never gives anything by itself.
6. **Plain words for Liis, exact words for Jonas.** The page shows plain words; the SQL, the
   SQLSTATE and the IDs sit under "Details" or in mono where Jonas needs them.

## Decisions (provisional)

| # | Decision | Why |
|---|---|---|
| DB-1 | A database opens on its own page (`/t/<space>/databases/<id>`), not in the details panel. Tabs: Overview, Query, Columns, Agent access, Query log, Backups. | Six areas of work do not fit a 360 px panel; the details panel keeps the facts and an "Open database" button. |
| DB-2 | The Versions tab of a database is called "Settings history". | Manifest versions are not backups of the data; the word "version" would promise a restore of rows. |
| DB-3 | The query editor starts in Read only on every page load; "Can change data" is a switch the person turns on each time. | A write by habit on live data is the most likely accident. |
| DB-4 | A person's own query that deletes or replaces data gets a confirm in the UI only (STEP2 open question 7, recommended answer). The UI checks the statement with a simple rule until the server can tell (C-20). | The server rule applies to agents only; people still need a moment to stop. |
| DB-5 | Recent queries are kept for the browser tab only (memory), never in browser storage. | Queries can hold personal values as literals; a shared computer must not keep them. |
| DB-6 | Stand-in chips show "person" and the first 6 hex characters of the pseudonym; the tooltip and copy give the full `p_` value. | Design 8.5 names the chip "person 8f3a91"; 6 characters are enough to tell values apart on a screen. |
| DB-7 | Copy and CSV export write markers as `[hidden: secret]` and `[person 8f3a91]`. | A pasted value must never pass as real data. |
| DB-8 | A grant link from an agent (`?give=...`) opens the write-grant prompt prefilled, capped to the contract limits, with the agent's reason marked as not checked. | Prompt injection: the agent's text is untrusted; the person must see who asks, for what and for how long. |
| DB-9 | In a masked branch, strings in Personal columns that match `p_` plus 12 hex characters render as stand-in chips, with a note above the grid. | In a branch the stand-ins are stored data, not markers; without the chip a person might take them for real IDs. |
| DB-10 | A request for a migration shows the classification review list in the same dialog, so the person approves the migration and confirms the classes with one button (design 8.5: "in the approval they already give"). Until C-15 lands, the dialog does it in two steps (approve now; confirm when the migration has run). | One decision, one place. The same component is reused by the publish approval of steps 3 and 4. |
| DB-11 | Restore to an earlier time defaults to "Restore as a new database". | It never loses anything; the person can compare and then delete one. |

## Contract needs found (UX numbering, continues C-7 to C-14 of `../decisions.md`)

These are not the STEP2 card IDs (C-1, C-2). Each one names the journey that needs it and a
provisional way to ship without it.

| # | Need | Journey | Proposed change | Without it |
|---|---|---|---|---|
| C-15 | Classes in the migration request | J10 | `DatabaseApproval` with reason `migration` carries `migration_name`, the `up` SQL and the new columns of a dry run with the author's classes; `decideDatabaseApproval` takes optional `classes` (the `ConfirmColumnClassesRequest` items) that the platform confirms in the same transaction as the approved migration. | Two steps in one dialog: approve now; after the migration ran, the dialog (or the Columns tab banner) asks to confirm the new columns. |
| C-16 | Web links that agents can give their user | J10, J11, J13 | `DatabaseApproval.review_url`; the hint of `write_grant_required`, and of `user_required` on `restoreDatabase` and `confirmColumnClasses`, carries the web URL of the right page (the grant prompt prefilled, the request, the Backups tab). | The agent names the database and the tab in words ("open garden, tab Agent access"). |
| C-17 | An agent can ask for a grant | J11 | `requestDatabaseAccessGrant` (MCP `request_database_access_grant`): a pending request with kind, minutes and reason that the person sees under "Waiting for you", like a statement request. | The agent asks in its chat and gives the C-16 link; the person fills the prompt. |
| C-18 | "Waiting for you" across a space | J10, J11 | List the pending requests that the caller may decide, across the space (`listDatabaseApprovals` at space level, or a filter), for a count in the rail and on the Agents page. | A count on each database row and on the database page only. |
| C-19 | The agent's user reads what the agent ran | J14 | `listDatabaseQueries`: the user an agent acts for sees that agent's entries with `view` (today: `manage` only). | A member with Can edit cannot see what their own agent ran; the page says "Only people who can manage *name* see the query log." |
| C-20 | "What would this query do" for people | J9 | `queryDatabase` with `check: true` (or an `explain` operation): runs nothing, answers the `reasons` and `estimated_rows` of a destructive statement, for every caller. | The UI's own simple check (DROP, TRUNCATE, DELETE or UPDATE without WHERE), no row count in the confirm. |
| C-21 | Plan room before create | J7 | `Usage` and `Limits` list databases per engine (count, total size). | "New database" fails late with `plan_limit_reached`. |
| C-22 | Names, not IDs, in database answers | J11, J14, J10 | `DatabaseAccessGrant.agent` and `granted_by` as `PrincipalRef` with `display_name`; `DatabaseApproval.decided_by` and `DatabaseColumn.confirmed_by` the same (C-7 applies to all step 2 answers). | The UI joins IDs with `listAgents` and `listMembers` (as in step 1). |
| C-23 | Who can read a database | J7, J8 | `Database.access_summary`: the counts (or the list) of people and agents with view and edit through the folder, so the page can say "Everyone who can see *Kalamaja* (8 people, 2 guests) can read this". | The page links to the folder's share dialog instead of a summary. |

## Notes for the architect panel

- `contracts/manifests/database.schema.json` (`extensions` description) still says that `anon`
  (PostgreSQL Anonymizer) is managed by Ruum; decision 0004-4 removed Anonymizer. Text only.
- `revoke_database_access_grant` uses the verb `revoke`, which the agent experience rules keep
  for credentials and links; step 1 uses `remove` for grants. `agent-experience.md` section 8
  proposes `remove_database_access_grant`. The tools are not built yet, so the rename is cheap.

## Open questions (provisional answers)

| Question | Provisional answer |
|---|---|
| Do branches show in Files? | Not as rows. The database row says "2 branches"; the branches are on the database page. A branch has its own page and breadcrumb (`Files > ... > garden > try-tags`). |
| Can Liis open the Query tab? | Yes, it is there for everyone with Can view, but her journeys never need it. The Overview links to it only as "Run a query". |
| Notifications for requests? | Not in step 2 (no notification contract). The agent tells its user in chat; the database row and page show the count (C-18 for a space-wide count). |
| Show sample values in the classification review? | Yes, for people who can manage (they may see every value): "Show 3 example values" runs a read-only `LIMIT 3` query as the person. Never for agents. |
