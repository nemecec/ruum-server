# Step 2 screens: databases

Status: `provisional`. Wireframes of the database screens (design 18.4.1). They show order,
hierarchy and content, not pixels: the look comes from `../design-system.md` and the words from
`../copy.md` 3.8. Journeys are in `journeys.md`. Widths: desktop 1280 px, phone 390 px.

## Who sees what

The page reads `Database.my_permissions` and shows only what the person may do (common state "No
permission" of `../journeys.md`). "Agent's user" is the person an agent acts for.

| Screen / action | Can view | Can edit | Can manage | Agent's user | Guest (view) |
|---|---|---|---|---|---|
| Overview (S2) | yes | yes | yes | | yes |
| Query, Read only (S3) | yes | yes | yes | | yes |
| Query, "Can change data" | | yes | yes | | |
| Secret values in results | hidden chip | hidden chip | real value | | hidden chip |
| Columns list (S4) | yes | yes, can suggest | yes, can confirm | | yes |
| Agent access lists (S6) | yes | yes | yes | | yes |
| Give access (S6, 4.23) | "See personal data" for own agents | own agents, both kinds | own agents, both kinds | required | own agents, "See personal data" |
| End a grant | | | yes | the one who gave it | |
| Decide a request (S5) | | requests about data from own agents | all requests (data and tables) | yes, with the right the query needs | |
| Query log (S9) | | | yes | own agents, with C-19 | |
| Backups list (S8) | yes | yes | yes | | yes |
| Name this moment | | yes | yes | | |
| Restore to an earlier time | | | yes | | |
| New branch (S7) | | masked, tables only | also full copy | | only with the policy |
| Settings (S10) | read only | read only | change | | read only |

## S1. New database dialog

Opened from "New" > "Database" in a folder. A form dialog (480 px; a sheet on the phone).

```
+------------------------------------------------------+
| New database                                     [x] |
|                                                      |
| Name                                                 |
| [ garden                                         ]   |
|                                                      |
| Kind of database                                     |
| (o) Postgres (recommended)                           |
|     For most apps. Tables, search, maps and vectors. |
| ( ) SQLite                                           |
|     One small file. For small apps with one writer.  |
|                                                      |
| > More options                                       |
|   Extensions (Postgres)                              |
|   [ ] postgis   Maps and places                      |
|   [ ] vector    Search by meaning (embeddings)       |
|   [ ] pg_trgm   Search with typos                    |
|   ...                                                |
|                                                      |
| Plan: Shared small, up to 1 GB, queries up to 30 s.  |
| (i) Everyone who can see Garden planner can read it. |
|     Agents read personal data as stand-ins.          |
|     [See who has access]                             |
|                                                      |
|                        [Cancel]  [Create database]   |
+------------------------------------------------------+
```

- "Create database" is disabled until there is a name. Enter submits.
- The extensions list is the manifest enum, each with one line (copy.md 3.8). SQLite hides it.
- Errors under the field (`name_taken`) or as a note above the footer (`plan_limit_reached`).

## S2. Database page, Overview

```
Files > Projects > Garden planner > garden
+--------------------------------------------------------------------------------+
| garden   [Ready]   Postgres 17                       [More v]  [Run a query]   |
| Overview | Query | Columns | Agent access (1) | Query log | Backups            |
+--------------------------------------------------------------------------------+
| WAITING FOR YOU                                                                |
| +----------------------------------------------------------------------------+ |
| | [key] Claude Code for Jonas wants to delete rows in plants                 | |
| |       About 1,240 rows. Asked 3 min ago. Ends in 23 h 57 min.   [Review]   | |
| +----------------------------------------------------------------------------+ |
| (!) 3 columns are not confirmed. They count as Personal until you confirm      |
|     them.                                                     [Review columns] |
|                                                                                |
| Size          120 MB of 1 GB  [=====-----------------------]                   |
| Backups       You can go back to any time since 30 Oct, 14:05.                 |
| Your access   You can read and change data, change the tables and see secret   |
|               values.                                                          |
| Created       6 Nov 2026 by Claude Code for Jonas                              |
| > Limits                                                                       |
|                                                                                |
| MIGRATIONS                                                          Show all > |
| 0003_add_tags        Applied   Claude Code for Jonas    Today, 14:20           |
| 0002_add_plants      Applied   Jonas Kask               Yesterday, 18:02       |
|                                                                                |
| BRANCHES                                                        [New branch]   |
| garden branch 1      Masked copy   Jonas Kask   12 MB   Today, 14:10           |
+--------------------------------------------------------------------------------+
```

- The tab count "(1)" is the number of requests this person may decide.
- "More" menu: "Settings" (S10), "Settings history" (the step 1 Versions panel, renamed),
  "Copy", "Move", "Rename", "Move to trash", "Copy ID". "Copy" of a database says before it
  runs: "The copy gets masked data: personal values become stand-ins and secrets are removed."
  (or "a full copy" for a person who can manage, with the choice).
- On a branch page the header shows the branch banner (S7) on every tab and no "New branch".
- Phone: the tabs become a horizontal scroll list with the current tab in view; the facts stack.

## S3. Query

```
+---------------------------+----------------------------------------------------+
| Tables            [Find]  | (o) Read only   [ ] Can change data                |
| v plants                  | +------------------------------------------------+ |
|     id        int8        | | 1 SELECT family, count(*)                       | |
|     name      text        | | 2 FROM plants WHERE planted > $1                | |
|     family    text        | | 3 GROUP BY family                               | |
|     owner     text  [P]   | +------------------------------------------------+ |
|     token     text  [S]   | Values   $1 [ 2026-05-01        ]                  |
| > customers               | Put values here, not in the query. Ruum never     |
| > tags                    | stores them.                                       |
|                           |                       [Copy v]  [Run query  Cmd+Enter]|
|                           +----------------------------------------------------+
|                           | family          count                              |
|                           | text            int8                               |
|                           | Rosaceae        12                                 |
|                           | Lamiaceae        7                                 |
|                           | 2 rows · 34 ms · Read only                         |
+---------------------------+----------------------------------------------------+
```

- `[P]` and `[S]` are the privacy badges (4.21): Personal and Secret; Normal has no badge;
  "not confirmed" adds a dashed outline.
- With "Can change data" on, the editor frame uses the warning style and the run button reads
  "Run and change data".
- Phone: the tables list moves behind a "Tables" button (a sheet); the editor is full width;
  the grid scrolls sideways inside its region.

## S4. Columns

```
+--------------------------------------------------------------------------------+
| Privacy classes decide what agents see. Normal: the real value. Personal: a    |
| stand-in, the same for the same value. Secret: hidden.                         |
| [All columns v]  [Only not confirmed (3)]                                      |
|                                                                                |
| plants                                                                         |
| Column     Type    Privacy                    Suggested                        |
| name       text    Normal     (confirmed)                                      |
| owner      text    Personal   (not confirmed) Personal: looks like a name      |
| token      text    Secret     (confirmed)                                      |
| customers                                                                      |
| email      text    Personal   (not confirmed) Personal: an email address       |
| notes      text    Personal   (not confirmed) Normal: short notes             |
|                                                                                |
|                                                  [Review 3 columns]           |
+--------------------------------------------------------------------------------+
```

- "Review 3 columns" opens the classification review list (4.22) in a dialog with the button
  "Confirm 3 columns". People who can manage only; others see "Ask *owner name* to confirm
  these."
- A column that went back to not confirmed after a rename shows the reason: "Renamed from
  *mail* on 6 Nov" (from `unconfirmed_reason`, when the API gives it).

## S5. Request dialog (a statement or a migration)

```
+------------------------------------------------------------+
| Review the change from Claude Desktop                  [x] |
| [key] Claude Desktop for Liis Tamm · asked 14:02 ·         |
| ends in 23 h 58 min                                        |
|                                                            |
| Adds the table customers with 4 columns.                   |
| > Show the migration (SQL)                                 |
| (i) Ruum records a restore point first. If the change      |
|     fails, nothing changes.                                |
|                                                            |
| Who can see the new columns                                |
| Normal: agents see the real value. Personal: agents see a  |
| stand-in. Secret: only people who can manage see it.       |
|                                                            |
| customers.name    text   ( ) Normal (o) Personal ( ) Secret|
|   Suggested by Claude Desktop: a person's name             |
| customers.email   text   ( ) Normal (o) Personal ( ) Secret|
|   Suggested by Claude Desktop: an email address            |
| customers.phone   text   (o) Normal ( ) Personal ( ) Secret|
|   (!) Agents will see the real values of phone.            |
| customers.since   date   (o) Normal ( ) Personal ( ) Secret|
|                                                            |
|           [Deny]   [Approve and confirm 4 columns]         |
+------------------------------------------------------------+
```

A statement request (reason `drop`, `truncate`, `delete_without_where`, `update_without_where`,
`schema_change`) has no column list; its body is:

```
| Claude Code wants to delete every row of plants.           |
| About 1,240 rows. The query has no WHERE.                  |
| DELETE FROM plants                                (mono)   |
| Values: [person 8f3a91] [value 2]                          |
| If you approve, Claude Code can run exactly this query     |
| once, in the next 15 minutes. It still needs write access. |
|                                  [Deny]   [Approve]        |
```

## S6. Agent access

```
+--------------------------------------------------------------------------------+
| Agents read garden, with personal data as stand-ins and secrets hidden.        |
| Extra access is for a short time, and you can end it at any time.              |
|                                                               [Give access]    |
| WAITING FOR YOU                                                                |
| (request cards, as on the Overview)                                            |
|                                                                                |
| ACCESS NOW                                                                     |
| [key] Claude Code for Jonas   Change data   until 15:42 (58 min left)          |
|       Restore point 14:42 · Used 4 times                           [End now]   |
|                                                                                |
| EARLIER                                                                        |
| [key] Claude Code for Jonas   See personal data   Ended at 12:00               |
| [key] Claude Code for Jonas   Change data   Gave it back at 11:10              |
|       [Go back to before this access]                                          |
| REQUESTS, DECIDED                                                              |
| DELETE FROM plants WHERE ...   Approved by Jonas Kask 15:05 · Used             |
+--------------------------------------------------------------------------------+
```

"Give access" opens the write-grant prompt (4.23). The link `?give=...` opens the same prompt
prefilled; `?request=<id>` opens the request dialog (S5).

## S7. New branch dialog and branch banner

```
+------------------------------------------------------+
| New branch of garden                             [x] |
| A branch is a copy of the database to try changes.   |
| Agents can change data in a masked branch without    |
| asking.                                              |
| Name   [ garden branch 1                         ]   |
| Data                                                 |
| (o) Masked copy (recommended)                        |
|     Personal values become stand-ins, secret values  |
|     are removed. Personal columns that are not text  |
|     become text.                                     |
| ( ) Tables only                                      |
|     The tables and columns, no rows.                 |
| ( ) Full copy                                        |
|     Real data, also personal values. Agents need     |
|     write access to change it.                       |
| From   (o) Now  ( ) An earlier time [date] [time]    |
|                          [Cancel]  [Make branch]     |
+------------------------------------------------------+
```

Branch banner (an info note at the top of every tab of a branch):
"Branch of *garden* · masked copy · made by Jonas Kask at 14:10. Agents can change data here
without asking." For a full copy: "... · full copy · ... Agents need write access to change data
here."

## S8. Backups

```
+--------------------------------------------------------------------------------+
| You can go back to any time since 30 Oct, 14:05.                               |
|                                  [Name this moment]  [Restore to an earlier time]|
| TODAY                                                                          |
| 14:42  Before write access for Claude Code   Claude Code for Jonas  [Restore...]|
| 11:20  Before import                          Marko Saar             [Restore...]|
| YESTERDAY                                                                      |
| 18:02  Before migration 0002_add_plants       Jonas Kask             [Restore...]|
| Restore points stay for 7 days, like the backups.                              |
+--------------------------------------------------------------------------------+
```

"Restore..." on a row opens the restore picker (4.25) with that point chosen.

## S9. Query log

```
+--------------------------------------------------------------------------------+
| Who [Everyone v]  What [All v]  Outcome [All v]                                |
| Ruum never stores the values in queries. They show as value 1, value 2.        |
| 15:12  [key] Claude Code for Jonas  Changed data                               |
|        UPDATE plants SET family = [value 1] WHERE id = [value 2]               |
|        Changed 12 rows · 8 ms · with write access                              |
| 15:10  [key] Claude Code for Jonas  Read                                       |
|        SELECT id, family FROM plants WHERE family IS NULL                      |
|        12 rows · 4 ms                                                          |
| 15:02  [key] Claude Code for Jonas  Changed data                               |
|        DELETE FROM plants                                                      |
|        Waiting for you                                                         |
|                                                             [Show more]        |
+--------------------------------------------------------------------------------+
```

A row opens the query details sheet (J14 step 3). The `$L1`, `$L2` placeholders of the stored
statement render as the chips "value 1", "value 2"; parameters render as stand-in chips.

## S10. Settings

A read-only summary for everyone, a form for people who can manage: "Kind of database" (fixed
after create: "Can't change after the database is made."), "Extensions" (Postgres), "Plan"
(fixed in step 2), "Backups: 7 days". Saving makes a new settings version: toast "Saved. Ruum is
applying the new settings." and the status "Updating". An agent's change shows in "Settings
history" with the agent mark.

## States per screen

Every screen has the common states. Screen-specific ones:

| Screen | Specific states |
|---|---|
| S1 | Plan full (C-21 or after the click); guest without the policy (the menu item is disabled, the dialog never opens). |
| S2 | Setting up, Updating, Restoring, Needs attention, Paused (trash / over limit), Didn't start; branch banner; nothing waiting (section hidden). |
| S3 | No tables; running; no rows; cut result; write mode; masked branch note; secret chips for people without Can manage. |
| S4 | All confirmed ("All columns are confirmed."); not confirmed after a rename; a suggestion from an agent vs. from the rules ("Suggested by Ruum: the name looks like an email column"). |
| S5 | Ended, already decided, read-only for people who may not decide; migration with no new columns; two-step confirm (DB-10). |
| S6 | No grants; no agents; agent paused or Can view only; link for another person's agent; a link asking over the limit (capped). |
| S7 | Too large for a branch; full copy disabled; time outside the window. |
| S8 | No backup yet; restore running; restore failed. |
| S9 | No queries; filter with no match; tab hidden without the right. |
| S10 | Read only; Updating after save; `validation_failed` on an extension that the cluster does not allow. |
