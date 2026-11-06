# Step 2 journeys: databases

Status: `provisional`. The eight journeys of step 2 (design 18.4.1, card U-1), numbered after the
six of step 1 (`../journeys.md`). Each journey has the persona, the steps with the screen of each
step, the success criteria (the measures for the synthetic user test, design 18.9) and the
states. Names follow `../ia.md`, components follow `../design-system.md` (4.19 to 4.26), screens
follow `screens.md`, and the strings in quotes are in `../copy.md` (3.8).

The common states of `../journeys.md` (loading, empty, error, no permission, not found, offline,
session ended) apply to every screen. Each journey lists only what differs. Every journey must
work at 1280 px and 390 px, light and dark, with the keyboard only and with a screen reader.

The rules of the contract that shape these journeys (`docs/STEP2.md`, permission table):

- Read-only queries need Can view; changes need Can edit; settings, migrations on live data,
  restores and secret values need Can manage.
- An agent reads Personal columns as stand-ins and Secret columns as hidden, in every part of a
  query. It changes live data only with write access, which only its own user gives, for at most
  24 hours. A query of an agent that deletes or replaces data, and an agent's migration on live
  data, wait for a person (a request).
- Restore to an earlier time and confirming classes are for people only.

## J7. Create a database for a project

**Personas:** Jonas (personal space) and Marko (team space). **Goal:** a database next to the
project's files, ready to use, with a clear idea of who can read it.

### J7a. Personal space (Jonas)

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens the folder "Projects/Garden planner" and clicks "New", then "Database" | Folder; "New" menu | "New" is a menu button with "Folder" and "Database" (step 1 had "New folder"; the menu keeps "Folder" first). Shown only with Can edit on the folder. |
| 2 | Fills in the dialog | New database dialog (screens.md S1) | Name (placeholder "garden"); "Kind of database": "Postgres (recommended)" preselected, "SQLite"; each with one line of meaning. "More options" (closed): extensions (Postgres only). The plan line: "Shared small: up to *1 GB*, queries up to *30 s*." The access line: "Everyone who can see *Garden planner* can read it. Agents read personal data as stand-ins." |
| 3 | Clicks "Create database" | Database page, Overview (S2) | The API answers at once (`202`); the page opens with the status badge "Setting up" and the line "Ruum is setting up *garden*. This takes a few seconds." The page polls `getDatabase` (1 s, then 2 s, 4 s, at most 10 s apart). |
| 4 | Waits | Overview | Status turns to "Ready". Toast "*garden* is ready." Focus stays where it was; the status region announces "Ready" (polite). The Overview shows "Next: run a query, or let an agent add tables." with "Run a query" and "Connect an agent". |
| 5 | Goes back to the folder | Folder | The row shows the database icon, the name, "Ready" (only states other than Ready show a badge), the size, and Changed "Just now, Jonas". |

### J7b. Team space (Marko)

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens "Projects/Kalamaja" in the team space Studio Linn; "New", "Database" | Folder; New database dialog | Same dialog. The access line names the folder's access: "Everyone who can see *Kalamaja* can read it, also guests. People who can edit it can change data." with the link "See who has access" (opens the folder's share dialog). With C-23 the line gives counts ("8 people, 2 of them guests"). |
| 2 | Picks "SQLite", names it "tracker", creates it | Overview | As J7a steps 3 and 4. |
| 3 | Helena (guest with Can view on Kalamaja) opens the folder | Folder | She sees "tracker" and can open it and run read-only queries; she sees no "New" menu item "Database": guests can't create databases in this space. If she opens the "New" menu, "Database" is there but disabled with "Guests can't create databases in this space. An admin can turn this on in Sharing rules." |
| 4 | Marko decides guests may create databases | Space settings, Sharing rules | New switch "Guests can create databases" with helper "Their databases, branches and copies count against *Studio Linn*'s plan." (TenantPolicy `guests_create_resources`). |

An agent can create a database too (`create_database`). Then the row and the Overview show the
agent mark: "Created by Claude Code for Jonas".

**Success criteria**

- Jonas creates a database and reaches "Ready" in under 60 s, with no help and no SQL.
- Marko can say, from the dialog alone, who will be able to read the new database.
- Helena understands why she can't create one, and who could change that.
- No unknown word on any screen of the journey (manifest, provisioning, node, tenant).

**States**

| State | What the person sees |
|---|---|
| Loading | The dialog opens at once; the plan line shows a skeleton until the plan is known. |
| Empty | Not possible: a new database starts empty, and the Overview says what to do next (step 4). |
| Error | Name taken: under the field (`name_taken`). Plan full (`plan_limit_reached`): in the dialog, "You have *3* Postgres databases, the most your plan allows. Delete one you don't use, or change your plan." (with C-21 the dialog says it before the click and disables "Create database"). Didn't start (`failed`): Overview status "Didn't start" with the reason in words and "Try again" (a new manifest version with the same settings) or "Move to trash". |
| No permission | Can view only: no "Database" in the "New" menu (no "New" menu at all when nothing can be made). Guest without the policy: the disabled item with its reason (step 3). |
| Offline | "Create database" disabled with "You're offline". A database that is setting up keeps its badge; the page polls again when back online. |

## J8. See the status of a database

**Personas:** Marko and Liis. **Goal:** answer "is it working, how full is it, who can do what,
and is anything waiting for me?" in one look.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Sees the database in Files | Folder | The row badge shows states that need attention: "Setting up", "Updating", "Needs attention", "Paused", "Didn't start". A count "1 request" (agent requests waiting for this person) and "3 columns to confirm" show in the name cell as quiet links. |
| 2 | Opens it | Overview (S2) | Header: name (h1), status badge, kind ("Postgres 17" or "SQLite"), and the page actions ("Run a query", "More"). Then the facts: Size "120 MB of 1 GB" with a bar; "Backups: you can go back to any time since 30 Oct, 14:05"; "Your access" (a sentence from `my_permissions`). |
| 3 | Reads what waits | Overview, "Waiting for you" | Request cards (4.24) and the banner "3 columns are not confirmed. They count as Personal until you confirm them." with "Review columns". Empty: the section is not shown. |
| 4 | Looks at recent changes | Overview, "Migrations" and "Branches" | The last 5 migrations (name, applied or failed, who, when) with "Show all"; the branches (name, data, who made it, size) with "New branch". |
| 5 | Checks limits | Overview, "Limits" (a disclosure) | "Longest query: 30 s. Rows per result: 10,000. Largest result: 10 MB. Largest database to branch: 1 GB." From `Database.limits`. |
| 6 | The database turns "Needs attention" | Overview | The status line names the reason in words (copy.md 3.8, status reasons), never the raw engine message; "Details" holds the reason code and the time. |

**Success criteria**

- Liis can say whether the plant list "works" and whether her Claude is waiting for her, within
  10 s of opening the page.
- Marko finds the size against the plan and the backup window without opening a second page.
- Every state other than Ready says what it means and what to do, in words.

**States**

| State | What the person sees |
|---|---|
| Loading | Skeleton header and facts; the tabs are usable at once. |
| Setting up / Updating | The badge with a small spinner (no spinner with reduced motion), the line "Ruum is setting up *garden*..." or "Ruum is applying new settings...", or "Ruum is restoring *garden* to *5 Nov, 14:00*. It answers no queries until this is done." Query and Backups actions are disabled with that reason. |
| Paused (`suspended`) | In the trash: "*garden* is in the trash. It answers no queries. Restore it from the trash to use it again." Over a hard limit: "*garden* is over your plan's limit of *1 GB*. It answers no queries until you free some space or change your plan." |
| Didn't start (`failed`) | The reason in words, "Try again" and "Move to trash". |
| Error | The page failed to load: the common error note with "Try again". |
| No permission | A person without access: the common "You don't have access to this". |
| Offline | Facts stay; the status shows "Last checked 14:02" and is checked again when back online. |

## J9. Run a read-only query

**Persona:** Jonas (also Marko). **Goal:** answer a question from the data, safely, and
understand the values he can't see.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens the tab "Query" | Query (S3) | Left: the tables list (from `getDatabaseSchema`) with column names, types and privacy class icons. Right: the query editor (4.20) in "Read only", empty with the placeholder "SELECT * FROM plants LIMIT 20". Above the editor: "Read only" and the switch "Can change data" (only with Can edit). |
| 2 | Clicks the table "plants" in the list | Query | Inserts `SELECT * FROM plants LIMIT 100` when the editor is empty, else the name at the cursor. Column names insert at the cursor. |
| 3 | Types a query with a value: `SELECT family, count(*) FROM plants WHERE planted > $1 GROUP BY family` | Query | The editor finds `$1` (Postgres) or `?1` (SQLite) and shows a "Values" row under the editor: one field per placeholder, "Value for $1". A short helper: "Put values here, not in the query. Ruum never stores them." |
| 4 | Presses Cmd+Enter (or "Run query") | Query, result grid (4.21) | The button shows "Running..." and a "Stop" link after 2 s (stop = the request is dropped; the server timeout still applies). The result: a grid with the column header (name, type, privacy badge for Personal and Secret), rows, and the footer "12 rows · 34 ms · Read only". |
| 5 | Sees a hidden value | Result grid | In a Secret column, a person without Can manage sees the chip "hidden: secret" (tooltip: "Only people who can manage *garden* see secret values."). NULL shows as a faint "NULL". |
| 6 | Copies the result | Result grid, "Copy" menu | "Copy as CSV", "Copy as table" (tab-separated), "Download CSV". Markers become `[hidden: secret]` and `[person 8f3a91]`. Toast "Copied 12 rows." |
| 7 | Runs a query that changes data by mistake in Read only | Query | The server answers `read_only_query`; the note under the editor: "This query changes data. Turn on Can change data to run it." (with Can edit) or "You can only read *garden*." (Can view). Nothing ran. |
| 8 | Turns on "Can change data" and runs an UPDATE with a WHERE | Query | The editor frame turns to the warning style and the button reads "Run and change data". Footer "Changed 3 rows · 12 ms". After the run the switch stays on until the page is left (DB-3). |
| 9 | Runs `DELETE FROM plants` (no WHERE) | Confirm dialog | DB-4: "Delete every row of plants?" Body: "This query has no WHERE, so it changes every row. You can restore *garden* to the moment before, on the Backups tab." Button "Run and delete rows" (danger). With C-20 the body adds the count ("about 1,240 rows"). |

**Success criteria**

- Jonas runs a parameterised read query and copies the result in under 2 minutes.
- Asked "what is the value in the token column?", the synthetic user says it is hidden and who can
  see it, and never reads the chip as a value.
- No query changes data without the person turning on "Can change data" first.
- Phone: the editor and the result grid work (the grid scrolls sideways inside its region, the
  page does not).

**States**

| State | What the person sees |
|---|---|
| Loading | Tables list: skeleton rows; the editor is usable at once. Running: "Running..." in the button, the old result dimmed with `aria-busy`. |
| Empty | No tables yet: the tables list says "No tables yet. Tables come from migrations, or from a CREATE TABLE in Can change data." Result with no rows: "No rows match." with the column header kept. |
| Error | `query_failed`: an error note under the editor with the text for people (copy.md 4) and "Details" with the SQLSTATE and the sanitized message; the line of the error is marked when the server gives a position. `statement_timeout`, `statement_not_allowed`, two statements (`validation_failed`), `database_not_ready`, `rate_limited`: copy.md 4. Cut result: a note above the grid "Showing the first *10,000* rows, the most your plan allows in one result. Add a WHERE or LIMIT to see the rest." |
| No permission | Can view: no "Can change data" switch; a line "You can read *garden*. Ask *owner name* to change data." |
| Offline | "Run query" disabled with "You're offline"; the last result stays. |

## J10. Confirm the suggested column classes in a request

**Persona:** Liis (her Claude Desktop keeps the "Plant list" database). **Goal:** approve the
change her agent asked for, with the right privacy class for each new column, without knowing
SQL.

Before this journey, Claude asked for write access and Liis gave it (J11). Claude then sent a
migration that adds a `customers` table; the server answered `202` with a request (reason
`migration`) and Claude told Liis in the chat: "I need your approval to add a customers table to
Plant list. Open Plant list in Ruum, tab Agent access." (with C-16 a direct link).

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens "Plant list" | Overview | "Waiting for you" shows one request card: agent mark, "Claude Desktop for Liis wants to change the tables of *Plant list*", "Asked 2 minutes ago", "Ends in 23 h 58 min", "Review". |
| 2 | Clicks "Review" | Request dialog (4.24, S5) | Title "Review the change from Claude Desktop". The change in words: "Adds the table customers with 4 columns." The SQL is under "Show the migration (SQL)" (closed). "Ruum records a restore point first. If the change fails, nothing changes." |
| 3 | Reads the columns | Request dialog, classification review list (4.22) | One row per new column: name, type, the suggested class preselected (a radio group Normal / Personal / Secret), and the reason: "Suggested by Claude Desktop: looks like an email address." A line above the list explains the three classes in plain words (copy.md 3.8). |
| 4 | Changes one class | Classification review list | `notes` is suggested Normal; Liis picks Personal. Picking a class lower than the suggestion (Personal to Normal, Secret to anything) shows an inline note: "Agents will see the real values of *phone*." Picking higher shows nothing. |
| 5 | Approves | Request dialog | Button "Approve and confirm 4 columns" (primary). With C-15 one call does both. Without it (DB-10): the dialog approves, then shows "Approved. Claude Desktop can run the change in the next 15 minutes. Ruum will ask you to confirm the columns once it has run." and the classes stay as chosen in the dialog until the migration has run, then the dialog (if open) or the Columns banner asks "Confirm 4 columns" with her choices kept. |
| 6 | Sees the result | Overview | Toast "Approved. Claude Desktop can make the change now." When the migration has run: the Migrations list shows "0003_add_customers, applied, Claude Desktop for Liis" and the columns show as confirmed. |
| 7 | (Other path) Denies | Request dialog | "Deny" (secondary) asks for an optional reason: "Tell Claude Desktop why (optional)". Toast "Denied. Claude Desktop was told." |

Two related paths use the same list:

- **Columns of existing data.** An agent calls `suggest_column_classes`; the Columns tab (S4) and
  the Overview banner show "3 columns are not confirmed". "Review columns" opens the same list
  with the suggestions filled in and the button "Confirm 3 columns".
- **The digest** lists unconfirmed columns per database with a link to the Columns tab.

**Success criteria**

- Liis approves the change and confirms the classes in under 2 minutes, and can say what an agent
  will see of the email column ("a stand-in, not the real email").
- Liis never confirms a column as Normal without having seen the note that agents will see the
  real values.
- She can deny without fear: the synthetic user says nothing changed after a deny.
- Jonas can open the SQL in one click.

**States**

| State | What the person sees |
|---|---|
| Loading | The dialog opens with the header; the columns list shows skeleton rows. |
| Empty | A migration with no new columns: the list is not shown; the button is "Approve". |
| Error | Request ended (`expired`): "This request has ended. Claude Desktop can ask again." Already decided by someone else: "*Marko Saar* already approved this at 14:05." Confirm failed (`precondition_failed`, the columns changed): "The columns changed while you were looking. Review them again." with the list reloaded. Migration failed after approval: Overview Migrations row "failed" and the status note "The change from Claude Desktop failed. Nothing changed." |
| No permission | A person who may not decide (not the agent's user, without Can manage): the card shows the request read-only: "Only Liis Tamm or people who can manage *Plant list* can decide this." Confirm needs Can manage; a person with Can edit sees the suggestions and "Ask *owner name* to confirm these." |
| Offline | Approve, Deny and Confirm disabled with "You're offline". |

## J11. Give an agent write access for one hour

**Persona:** Jonas (also Liis). **Goal:** let Claude Code fix live data for a short time, know
exactly what it may do and until when, and have a way back.

Claude Code tried an UPDATE on live data and got `write_grant_required`. Its hint tells it to ask
Jonas. Claude says in the chat: "I need write access to garden to fix the 12 plants with a wrong
family. Give it on the garden page, tab Agent access." With C-16 the message carries a link that
opens the prompt prefilled (`?give=write_live&agent=<id>&minutes=60`).

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens "garden", tab "Agent access" | Agent access (S6) | Sections: "Waiting for you" (requests), "Access now" (active grants with a countdown), "Earlier" (ended grants). Button "Give access" (only for people who have agents with access to this database). |
| 2 | Clicks "Give access" (or follows the link) | Write-grant prompt (4.23) | A dialog. "Agent": a select of the person's own agents that can reach this database (prefilled from the link). "Access": "Change data" (write access) / "See personal data". "For": 15 minutes, 1 hour (default), 4 hours, 8 hours, 24 hours ("See personal data": at most 8 hours). "Reason (optional)". From a link, the agent's reason shows in a quoted block "Reason from Claude Code (Ruum didn't check this)". |
| 3 | Reads the summary | Write-grant prompt | "Claude Code can change data in *garden* until *15:42* today. Ruum records a restore point first, so you can go back to *14:42*. Queries that delete or replace data still ask you first." |
| 4 | Clicks "Give access for 1 hour" | Agent access | The grant row in "Access now": agent mark, "Change data", "until 15:42 (58 min left)", "Restore point 14:42" (a link to Backups), "End now". Toast "Claude Code can change data in *garden* until 15:42." |
| 5 | Claude works; Jonas watches | Agent access, Query log | The grant row shows "Used 4 times" (from the query log with C-19 or with Can manage). |
| 6 | Ends it early | Agent access, "End now" | No confirm (it only takes access away); toast "Ended write access for Claude Code." If Claude gives it back first: the row moves to "Earlier" with "Claude Code gave it back at 15:10". |
| 7 | The hour ends | Agent access | The row moves to "Earlier": "Ended at 15:42". Nothing is needed from Jonas. |
| 8 | (Later) Wants the data as it was before | Grant row in "Earlier", "Go back to before this access" | Opens the restore picker (J13) with the grant's restore point chosen. |

**Success criteria**

- Jonas gives write access for one hour in under 30 s from the agent's message.
- After giving it, he can say until when it lasts and how to undo the agent's changes.
- A prefilled link never gives access without the person's click; the synthetic user can say
  which agent asked and what for.
- Liis, with the same prompt, picks "1 hour" and does not pick "See personal data" by mistake (the
  two choices have different words and icons).

**States**

| State | What the person sees |
|---|---|
| Loading | Lists: skeleton rows. The prompt: the agent select shows a skeleton until `listAgents` answers. |
| Empty | No grants yet: "No agent has extra access to *garden*. Agents can read it, with personal data as stand-ins." No agents at all: "Give access" is replaced by "Connect an agent". |
| Error | The agent's access is Can view only: "Change data" disabled with "*Claude Code*'s access is Can view. Change its access on the Agents page first." Agent paused: "*Claude Code* is paused. Resume it first." The link asks for more than the limit (for example 48 hours): the prompt caps it to 24 hours and says "Claude Code asked for 48 hours. The most is 24 hours." The link names an agent of another person: "This link is for an agent of *Marko Saar*. Only Marko can give it access." (nothing to fill in). |
| No permission | Only the agent's user can give access. Others see the lists read-only (Can view) and no "Give access". "End now" shows for the user who gave it and people who can manage. A person with Can view only can't give "Change data": the choice is disabled with "You can only read *garden*, so you can't give write access." |
| Offline | "Give access" and "End now" disabled with "You're offline". The countdown keeps running from the end time it knows. |

## J12. Make a branch

**Persona:** Jonas. **Goal:** a copy of "garden" where Claude Code may try a migration freely,
without real personal data.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Overview, "Branches", "New branch" | New branch dialog (S7) | One line: "A branch is a copy of the database to try changes. Agents can change data in a masked branch without asking." Fields: "Name" (default "*garden* branch 1"); "Data": "Masked copy (recommended)", "Tables only", "Full copy" (each with one line); "From": "Now" or "An earlier time" (a date and time inside the backup window). |
| 2 | Keeps "Masked copy" | New branch dialog | Under the choice: "Personal values become stand-ins and secret values are removed. Personal columns that are not text become text in the branch." |
| 3 | Clicks "Make branch" | Branch page (Overview) | Status "Setting up", then "Ready". A banner on every tab of the branch: "Branch of *garden* · masked copy · made by Jonas Kask at 14:10. Agents can change data here without asking." |
| 4 | Tells Claude Code to try the migration on the branch | (Claude Code) | Claude uses `create_database_branch` or the branch ID; on a masked branch it writes and migrates without write access or requests. |
| 5 | Looks at the data | Branch, Query | Stand-ins stored in the branch render as chips (DB-9) under the note "This branch has masked data. Values like *person 8f3a91* are stand-ins, not real data." |
| 6 | Deletes the branch when done | Branch page, "More", "Move to trash" | As for any item (toast with "Undo"). The database row's branch count goes down. |

**Success criteria**

- Jonas makes a masked branch in under 45 s and can say what data it holds.
- The synthetic user never picks "Full copy" by accident: it is the last choice, and it says
  "Real data, also personal values. Agents need write access to change it."
- On a branch page, the person can always tell that it is a branch and of what.

**States**

| State | What the person sees |
|---|---|
| Loading | The dialog opens at once; "From: An earlier time" shows the window when it is known. |
| Empty | No branches: "No branches yet. Make one to try changes without touching *garden*." with "New branch". |
| Error | Too large (`database_too_large`, also before the click from `size_bytes` and `limits`): "*garden* is *2.3 GB*. Branches work for databases up to *1 GB*. Make a branch with tables only." with "Tables only" selected. Time outside the window: under the field, "Pick a time after *30 Oct, 14:05*." Guest without the policy: as J7b. A branch of a branch: "New branch" is not shown on a branch page (STEP2 open question 10). |
| No permission | Can view: no "New branch". "Full copy" needs Can manage: disabled with "Only people who can manage *garden* can make a full copy." |
| Offline | "Make branch" disabled with "You're offline". |

## J13. Restore to an earlier time

**Persona:** Marko. **Goal:** an import went wrong at 11:20; get the tracker back to how it was at
11:00, without losing anything.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens "tracker", tab "Backups" | Backups (S8) | The window: "You can go back to any time since *30 Oct, 11:02*." The list of restore points, newest first, grouped by day: label or reason ("Before import", "Before write access for Claude Code", "Before migration 0004_add_status", "Before a restore"), time, who. Buttons "Name this moment" and "Restore to an earlier time". |
| 2 | Clicks "Restore to an earlier time" | Restore picker (4.25) | A dialog. "Go back to": "A restore point" (a list) or "A time" (date and time fields, with the time zone shown: "Tallinn time (UTC+2)"). Default: "A time", today, empty time. |
| 3 | Enters 11:00 | Restore picker | Validation inside the window. A line under the time: "That's *20 minutes before* the restore point *Before import* (11:20)." |
| 4 | Chooses where | Restore picker, "Restore how" | "As a new database next to *tracker* (recommended)": name "tracker (restored 6 Nov 11:00)"; "Replace the data in *tracker*": "*tracker* answers no queries while Ruum restores it, usually a few minutes. Ruum records a restore point of the data now, so you can undo this." |
| 5 | Keeps "As a new database", clicks "Restore" | Folder or new database page | The new database row appears with "Setting up". Toast "Restoring *tracker* to 6 Nov, 11:00 as a new database." [Show]. When ready: "*tracker (restored 6 Nov 11:00)* is ready." |
| 6 | (Other path) Replaces the data | Confirm dialog | Title "Replace the data in *tracker*?" Body: "The data goes back to 6 Nov, 11:00. Changes after that are kept in the restore point *Before a restore* (now), so you can undo this." Button "Replace the data" (primary, it can be undone). Then the status "Updating" with "Ruum is restoring *tracker*..."; when done, toast "Restored *tracker* to 6 Nov, 11:00." [Undo]. "Undo" opens the picker with the "Before a restore" point and "Replace the data" chosen. |
| 7 | Names a moment for later | Backups, "Name this moment" | A small dialog: "Name" (placeholder "Before import"). "Restore points stay for 7 days, like the backups." Toast "Named this moment *Before import*." |

**Success criteria**

- Marko restores to a chosen time as a new database in under 90 s, and says before the click
  that the current tracker stays as it is.
- Choosing "Replace the data", he can say how to undo it.
- Times are never ambiguous: the time zone is on the screen.

**States**

| State | What the person sees |
|---|---|
| Loading | Restore points: skeleton rows; the window line shows a skeleton. |
| Empty | No backup yet (`pitr_window` is null, a new database): "Ruum makes the first backup within a few minutes. After that you can go back to any time in the last 7 days." Restore buttons disabled with that reason. No named points: the list shows only automatic points; when none: "No restore points yet. Ruum makes one before each migration and each write access." |
| Error | Time outside the window: under the field. Restore failed (`database.restore_failed`): the target's status "Didn't start" (new database) or the note "The restore didn't work. *tracker* is as it was." (in place). Restore already running: "A restore of *tracker* is already running." |
| No permission | Restore needs Can manage: the button is not shown; a line "Only people who can manage *tracker* can restore it." "Name this moment" needs Can edit. Agents can't restore; an agent that tries tells its user to do it here. |
| Offline | Both buttons disabled with "You're offline". |

## J14. See what an agent ran

**Persona:** Jonas (also Marko as admin). **Goal:** after Claude Code's hour of write access, see
which queries it ran, what changed, and whether anything failed.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens "garden", tab "Query log" | Query log (S9, 4.26) | Rows newest first: time, who ("Claude Code for Jonas" with the agent mark, or a person), "Read" or "Changed data" badge, the query on one line in mono, the outcome ("12 rows", "Changed 12 rows", "Failed: *code in words*", "Stopped: took too long", "Waiting for you"), the time it took. Filters: "Who" (everyone, people, each agent), "What" (all, reads, changes), "Outcome" (all, failed). |
| 2 | Filters "Who: Claude Code" and "What: changes" | Query log | The list narrows; the filter state is in the URL (`?actor=<id>&mode=write`), so Jonas can send the link. |
| 3 | Opens one row | Query details (a side sheet) | The full query with values shown as chips "value 1", "value 2" (literals are never stored); the parameters as stand-in chips; "With write access until 15:42" (link to the grant); "Approved by Jonas Kask at 15:05" when a request allowed it; the query ID with a copy button. A line: "Ruum never stores the values in queries, so they show as value 1, value 2." |
| 4 | Checks the migrations | Overview, Migrations, "Show all" | Applied, failed and rolled back migrations with who and when; a failed one shows the error in words ("Details" for the SQLSTATE). |
| 5 | Checks from the agent's side | Agents, "Claude Code", Activity | Step 1's Activity shows the database events of the journal (card C-2): "Got write access to *garden* for 1 hour", "Applied the migration *0003_add_tags* in *garden*", "Made the branch *garden branch 1*". Each links to the database. Queries are not journal entries; the Activity says "Queries are in each database's Query log." |

**Success criteria**

- From the database page to "which queries did Claude Code run in the last hour, and which
  failed" in under 30 s.
- The synthetic user understands that "value 1" stands for a value Ruum did not keep, not a
  value of 1.
- Every row names the agent and the person it worked for.

**States**

| State | What the person sees |
|---|---|
| Loading | Skeleton rows. |
| Empty | "No queries yet. Every query on *garden*, by people and agents, shows up here." Filter with no match: "No queries match these filters." with "Clear filters". |
| Error | The common error note with "Try again". |
| No permission | The tab is shown only to people who can manage the database (and, with C-19, to the agent's user for their own agents). A person without it who opens the URL: "Only people who can manage *garden* see the query log." |
| Offline | The list stays; filters work on what is loaded. |

## Journey measures for the synthetic user test

| Journey | Task given to the persona agent | Pass when | Target (provisional) |
|---|---|---|---|
| J7 | "Make a Postgres database called garden in Projects/Garden planner." (Jonas) / "Make a database for the Kalamaja project. Who will be able to read it?" (Marko) | Database ready; answer names the folder's people and guests | 95% / 90% success |
| J8 | "Is the plant list working? Is your Claude waiting for you?" (Liis) | Correct answer for both | 90% success, under 10 s of reading |
| J9 | "How many plants of each family were planted after 1 May? Then tell me what is in the token column." (Jonas) | Correct counts; says the token is hidden | 90% success, no write |
| J10 | "Claude wants to add a customers table. Approve it, and make sure your Claude can't read your clients' phone numbers." (Liis) | Approved; phone and email are Personal (or Secret) | 85% success, no column lowered without the note seen |
| J11 | "Let Claude Code change data in garden for one hour." (Jonas) | Write access, 60 minutes, to the right agent | 95% success, under 30 s |
| J12 | "Make a copy of garden where Claude can try changes, without real personal data." (Jonas) | Masked branch | 95% success, zero full copies |
| J13 | "The import at 11:20 broke the tracker. Get the data from 11:00 back without losing today's other changes." (Marko) | Restored as a new database (or in place with the undo point named in the answer) | 85% success |
| J14 | "Which queries did Claude Code run in garden in the last hour, and did any fail?" (Jonas) | Correct list and failures | 90% success, under 30 s |
