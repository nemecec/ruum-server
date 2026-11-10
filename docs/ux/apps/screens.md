# Step 3 screens: web apps, issues, templates, reports

Status: `provisional`. Wireframes of the step 3 screens (design 18.4.1). They show order,
hierarchy and content, not pixels: the look comes from `../design-system.md` (and the Graphite
tokens), the components from `components.md`, and the words from `copy.md` 3.9. Journeys are in
`journeys.md`. Widths: desktop 1280 px, phone 390 px.

## Who sees what

The app page reads `App.my_permissions` (`use`, `deploy`, `publish`, `read_telemetry`,
`configure`, `share`) and shows only what the person may do. "Agent's user" is the person an
agent acts for. "App role only" is a person with an app role and no Ruum access on the node.

| Screen / action | App role only | Can view (Can use) | Can edit | Can manage | Agent's user |
|---|---|---|---|---|---|
| Use the live app (its address) | yes | yes | yes | yes | |
| App page, Overview (A3) | no (not in Files) | facts, "Open app" | + traffic, errors, previews, requests | + secrets missing | |
| Open a preview | | | yes | yes | |
| Versions (A4), Go back | | list only | yes | yes | |
| Preview page (A5), Make a preview | | | yes | yes | |
| Publish (A6) | | | yes (and Can manage on every database a migration changes) | yes | |
| Decide a publish request (A6) | | | | yes | yes (with the rights the publish needs) |
| Logs (A7), Issues (A8) | | | yes | yes | |
| Look up a person (A7) | | | yes, people only | yes, people only | |
| Access (A9), Give access (A10) | | list only | yes, within own rights on the target | yes | |
| Share, Ruum access (A12) | | | with can share | yes | |
| Share, app roles (A12) | holders of a role that can give app roles | | with can share | yes | |
| Settings (A15) | | | read only | change | |
| Who can use it (Settings) | | | read only | change; "Anyone on the internet" also needs owner or admin and Ruum's approval | |
| Secrets: enter value, Previews get this value | | | names and states | yes, people only | |
| Your own domain: add | | | | owner or admin, Ruum's approval, people only | |
| API keys: create | | | | people only | |
| Issue page (A11) | | yes | + occurrences, report attachments, Fix with an agent | yes | |
| File an issue | | with Can comment on the folder | yes | yes | |
| Ask a reporter to see fields | | | yes, people only | yes, people only | |
| Report page (A13) | any user of the app | any user of the app | yes | yes | |
| Receipt page (A14) | whoever has the link | | | | |

## A1. New project from template

Opened from "New" > "Project from template..." in a folder (`/t/<space>/templates?parent=...`).
A full-page dialog on desktop (900 px), a page on the phone.

```
+--------------------------------------------------------------------------------+
| New project from template                                                  [x] |
| In Projects                                                       [Change]     |
|                                                                                |
| FROM RUUM                                                                      |
| +--------------------------+ +--------------------------+ +-------------------+|
| | [app+db]                 | | [page]                   | | [table]           ||
| | Web app with a database  | | Static site              | | Internal tool     ||
| | and sign-in              | |                          | |                   ||
| | For apps where people    | | Pages, a blog or a       | | Lists and forms   ||
| | sign in and keep data.   | | portfolio. No server.    | | for your team.    ||
| | You get: an app, a       | | You get: an app, 3 issue | | You get: an app,  ||
| | Postgres database with   | | kinds.                   | | a database, an    ||
| | sample data, 4 issue     | |                          | | approver role.    ||
| | kinds.                   | |                          | |                   ||
| |                [Choose]  | |               [Choose]   | |        [Choose]   ||
| +--------------------------+ +--------------------------+ +-------------------+|
| FROM STUDIO LINN                                                               |
| +--------------------------+                                                   |
| | Client project kit  ...  |                                                   |
+--------------------------------------------------------------------------------+
```

After "Choose", the second panel (same dialog, "Back" at the top left):

```
+------------------------------------------------------+
| < Back       Internal tool                       [x] |
|                                                      |
| Project name                                         |
| [ Project tracker                                ]   |
|                                                      |
| Where      Projects                     [Change]     |
|                                                      |
| Who is the approver? (optional)                      |
| [ Name or email                                  ]   |
| You can do this later.                               |
|                                                      |
| (i) Everyone who can see Projects can see the new    |
|     project. The app is open only to people you      |
|     share it with.                                   |
|                                                      |
| Plan: 2 of 3 apps.                                   |
|                         [Cancel]  [Create project]   |
+------------------------------------------------------+
```

- Template cards are 4.37. The "You get" line comes from the template manifest.
- A placeholder field appears per role placeholder of the template, with the people picker of
  the share dialog. The label is "Who is the *placeholder*?"; the text of the placeholder is the
  template's (untrusted text, shown as text).
- The plan line shows "*2* of *3* apps" when the plan has a limit; at the limit, "Create
  project" is disabled with the `plan_limit_reached` copy.

## A2. Setup progress

`/t/<space>/templates/setups/<id>`. The page polls `getTemplateSetup` with `wait=30`.

```
Files > Projects > Project tracker
+--------------------------------------------------------------------------------+
| Setting up Project tracker                                                     |
| From the template Internal tool 1.2.0                                          |
|                                                                                |
| [done]  Making folders                                                         |
| [done]  Setting up the database tracker                                        |
| [done]  Adding sample data (masked)                                            |
| [spin]  Making the first preview of Project tracker                            |
|         Trying it in a browser                                                 |
| [ ]     Adding issue kinds                                                     |
| [ ]     Giving access                                                          |
|                                                                                |
| You can leave this page. Ruum keeps going.                                     |
+--------------------------------------------------------------------------------+
```

Done:

```
| Project tracker is ready to try!                                               |
| [Open the preview]   [Go to the project]                                       |
| (!) Nobody is the approver yet.                              [Choose people]   |
```

- The step list is 4.30 (the same list as a preview's steps). A step that runs a preview shows
  its current preview step as a second line.
- "Choose people" opens the share dialog of the app with the role preselected.

## A3. App page, Overview

```
Files > Projects > Project tracker > Project tracker
+--------------------------------------------------------------------------------+
| Project tracker  [Live]                                [Share]  [Publish]      |
| tracker.linn.ruumapps.net  [Copy] [Open app]                                   |
| Overview | Versions | Logs | Issues (3) | Access | Settings                    |
+--------------------------------------------------------------------------------+
| WAITING FOR YOU                                                                |
| +----------------------------------------------------------------------------+ |
| | [agent] Claude Code for Jonas asks to publish Favourites list              | |
| |         Asked 3 min ago. Ends in 23 h 57 min.                    [Review]  | |
| +----------------------------------------------------------------------------+ |
| (!) STRIPE_KEY has no value yet. The next publish waits for it. [Enter value]  |
|                                                                                |
| Live          Version 12, published 2 hours ago by Claude Code for Jonas       |
| Who can use   8 people in Studio Linn, 2 guests                [Share]        |
|               Approvers: Kadri, Tõnu                                           |
| Right now     Asleep. It wakes on the next visit, in 1 to 3 seconds.           |
|                                                                                |
| LAST 24 HOURS                                                                  |
| +----------------+ +----------------+ +----------------+                       |
| | Visits         | | Errors         | | Speed          |                       |
| | 1,240  ~~^~~~  | | 0.4%  __/\__   | | 320 ms  ~~~~   |                       |
| +----------------+ +----------------+ +----------------+                       |
|                                                                                |
| ISSUES                                                       See all issues >  |
| [Open] TypeError: plants is undefined  37 times, 12 people  since version 12   |
| [Back again] Failed to fetch /api/hours  4 times, 2 people                     |
|                                                                                |
| PREVIEWS                                                     See versions >    |
| Favourites list   Ready to try   by Claude Code for Jonas, 3 min ago  [Open]   |
+--------------------------------------------------------------------------------+
```

- Header: name (h1), the status badge (4.27) in a `role="status"` region, the address (4.28),
  page actions "Share" and "Publish" (primary, only when a ready preview exists and the person
  may publish; with several previews it opens a menu of them).
- "Who can use" needs C-25; without it the row is "People with access: [Share]".
- "Right now" needs C-30; without it the row is left out.
- Tiles are 4.34; hidden for Can view and before the first publish.
- The automatic rollback banner (APP-15) sits at the top of "Waiting for you".
- Phone: the tabs scroll sideways with the active tab in view; the tiles stack; "Share" and
  "Publish" move into the "More" menu next to the title, with "Publish" kept as the visible
  primary.

## A4. Versions

```
| PREVIEWS                                                                       |
| Favourites list     [Ready to try]   Claude Code for Jonas · 3 min ago         |
|                     Check: no errors                    [Open] [Publish] [...] |
| Fix map markers     [Getting ready: Starting]   Jonas · 1 min ago              |
| Try dark mode       [Didn't start: Checking app.yaml]   Jonas · yesterday      |
|                                                                                |
| LIVE VERSIONS                                                                  |
| Version 13  [Taken back]  Published 14:05 by Jonas. Ruum went back at 14:20.   |
|                                                       [See issues] [Publish]   |
| Version 12  [Live]        Access to tracker · 14:20 (went back by Ruum)        |
| Version 11                Sortable plant list · 5 Nov, 14:00 by Liis [Go back] |
| Version 10                ...                                       [Go back]  |
|                                                              [Show more]       |
```

- A preview row (4.29) links to its page (A5). The row menu: "Copy address", "Compare with
  live" (the change summary only), and nothing that deletes (previews end by themselves after
  *7* days without visits; "Ends in *6* days" shows in the row).
- A live row: the number, the label, the badge when not plain, who published and when, and
  "Go back" (Can edit). The dialog: journeys J18c.
- Expired previews are under "Ended previews" (a closed disclosure).

## A5. A preview

`.../apps/<id>/versions/<deployment id>`.

```
Files > ... > Project tracker > Versions > Favourites list
+--------------------------------------------------------------------------------+
| Favourites list  [Ready to try]                      [Open preview] [Publish]  |
| Preview of Project tracker · by Claude Code for Jonas · 3 min ago              |
| 7f3k2qpa--tracker.linn.ruumapps.net  [Copy]                                    |
+--------------------------------------------------------------------------------+
| STEPS                                                                          |
| [done] Reading files  [done] Checking app.yaml  [done] Connecting data         |
| [done] Updating tables  [done] Packaging  [done] Starting                      |
| [done] Trying it in a browser                                                  |
|                                                                                |
| BROWSER CHECK                                                                  |
| (ok) Ruum opened 5 pages in a browser: no errors.             [Show pages]     |
|                                                                                |
| WHAT CHANGES WHEN YOU PUBLISH                                                  |
| + Table favourites with 3 columns (in garden)                                  |
| + Log field plant_name                                                         |
| = Access, roles and secrets stay as they are.                                  |
|                                                                                |
| WHAT IT CAN REACH                                                              |
| garden (a masked copy)         can change data                                 |
| Photos/Garden                  can read                                        |
|                                                                                |
| > Details                                                                      |
|   Code: folder app/ (12 files, versions of 14:02)   Draft: favourites          |
|   Preview ID 01J...  [Copy]                                                    |
+--------------------------------------------------------------------------------+
```

- Steps are 4.30; the browser check is 4.30 too; the change summary is 4.31.
- A failed step opens by itself: the step's message (untrusted text: plain text, mono where it
  quotes a file), the file and line or the `app.yaml` place in words ("app.yaml, bindings, db"),
  and the people's copy of the hint (copy.md 3.9, "Step errors"). "Details" holds the code, the
  hint and `error.details` as JSON.
- A preview that waits on a database feature of step 2 shows the step "Connecting data" as
  waiting with the copy of `database_branches_not_ready`.

## A6. Publish dialog and publish request dialog

One component (4.33) in two modes. Width 640 px; a full-screen sheet on the phone.

**Publish (a person's own preview):**

```
+----------------------------------------------------------------------+
| Publish Favourites list?                                         [x] |
|                                                                      |
| [ preview picture (C-31) ]                     [Open preview ->]     |
| (ok) Ruum opened 5 pages in a browser: no errors.                    |
|                                                                      |
| WHAT CHANGES                                                         |
| + Table favourites with 3 columns, in garden                         |
| + Log field plant_name                                               |
|                                                                      |
| WHO CAN SEE THE NEW COLUMNS                                          |
| client_email   [Personal v]  Suggested: looks like an email          |
| plant_id       [Normal v]                                            |
| added_at       [Normal v]                                            |
| WHO CAN SEE THE NEW LOG FIELDS                                       |
| plant_name     [Normal v]   Suggested by Claude Code: a plant name    |
|                                                                      |
| It becomes version 13. People using Garden planner get it at once.   |
| Ruum records a restore point of garden first. You can go back to     |
| version 12 at any time; the table changes stay.                      |
|                                                                      |
|                                  [Cancel]  [Publish]                 |
+----------------------------------------------------------------------+
```

**Request (an agent asked):** the title is "Publish request from *Claude Desktop*", a by-line
with the agent mark ("*Claude Desktop* for *Liis* asked *3 min* ago. Ends in *23 h 57 min*."),
the label, then the same body. Footer: "Deny" (secondary, focused first) and "Approve and
publish" (or "Approve", APP-6). The button counts confirmations when there are any: "Approve and
publish, confirm *4* items".

- Secrets missing: a danger note above the footer and the primary button disabled (journeys
  J18, states).
- Removed access, removed roles and a rollback across a migration use the warning style in the
  change summary.
- The classification lists are 4.22 (step 2), reused as DB-10 promised.

## A7. Logs

```
| [Live v] [Last hour v] [All levels v] [Server and browser v] [ Search logs   ] |
| [Look up a person]                           Checking for new records every 5 s |
+--------------------------------------------------------------------------------+
| 14:22:05  ERROR  Server  order {order_id} failed for {customer}                 |
|                          order 1832 failed for [person 8f3a91]                 |
| 14:22:04  INFO   Browser page view /hours                                       |
| 14:21:58  WARN   Server  slow query on {table} took {ms} ms                     |
|                          slow query on hours took 1840 ms                      |
| 14:21:40  ERROR  Ruum    The app stopped and Ruum started it again.            |
|                                                               [Show older]     |
```

Record panel (right; a sheet on the phone):

```
| order {order_id} failed for {customer}          [x] |
| 14:22:05 · Error · Server · version 12 · /checkout  |
| FIELDS                                              |
| order_id    1832                    Normal          |
| customer    [person 8f3a91]         Personal        |
| card        [hidden: secret]        Secret          |
| [Show the whole request]                            |
| > Details: trace 4bf92f35...  [Copy] [Open trace]   |
```

- The log viewer is 4.32. The text of a record is untrusted: plain text only, no links, no
  markup; long lines wrap.
- "Look up a person" opens a small dialog (APP-14): Field (a select of Personal log fields),
  Value (a text field with `autocomplete="off"`), the helper, "Find records". The value is sent
  once and cleared from the field and from memory after the answer.
- "Server and browser" filter values: Server, Browser, Ruum.

## A8. Issues

```
| [Open v] [All sources v]                         Sort: [Last seen v]           |
+--------------------------------------------------------------------------------+
| [Open]       TypeError: plants is undefined                   ~~/\__  37 times |
|              Found by Ruum · since version 12 · last 2 min ago       12 people |
| [Back again] Failed to fetch /api/hours                       __/\~~   4 times |
|              Found by Ruum · since version 11 · last 1 hour ago       2 people |
| [Open]       Hours didn't save                                                 |
|              Reported by a user · 1 hour ago · assigned to [agent] Claude Code |
| [Open]       24 new errors after version 13                                    |
|              Found by Ruum · group                                     [Open]  |
```

- Rows are 4.35. Filters: status (Open includes Back again; Closed; All), source (Found by Ruum,
  Reported, Filed by people and agents).
- The tab label carries the count of open issues: "Issues (3)".

## A9. Access

```
| What Project tracker can reach. People who use the app don't get this access;  |
| the app does.                                                  [Give access]   |
|                                                                                |
| DATABASES                                                                      |
| tracker            can change data     given by Marko, 10 Nov     [Remove...]  |
| FILES AND FOLDERS                                                              |
| Projects/Kalamaja  can read            given by Marko, 10 Nov     [Remove...]  |
| OTHER APPS                                                                     |
| Invoices (API orders)  can call: orders.read  given by Marko      [Remove...]  |
|                                                                                |
| Previews use a masked copy of each database.                                   |
```

- Rows are 4.31 (binding rows). The name links to the item when the person can see it; else
  "an item you can't see".
- "Remove..." opens the same flow as "Give access", in reverse: it removes the lines from
  `app.yaml`, makes a preview and says the live app keeps the access until the person
  publishes.
- The list shows the live version. When a ready preview changes the access, a note: "The
  preview *Favourites list* changes this. [See the preview]".

## A10. Give access dialog

```
+------------------------------------------------------+
| Give Project tracker access                      [x] |
|                                                      |
| To                                                   |
| (o) A database                                       |
| ( ) A file or folder (read only)                     |
| ( ) Another app's API                                |
|                                                      |
| Database                                             |
| [ tracker  (Projects/Kalamaja)                 v ]   |
|                                                      |
| Access                                               |
| ( ) Can read                                         |
| (o) Can change data                                  |
| You can manage tracker, so you can give this.        |
|                                                      |
| Name in the code                                     |
| [ tracker ]                                          |
| The app reads it from RUUM_BINDING_TRACKER.          |
|                                                      |
| Ruum adds these lines to app.yaml and makes a        |
| preview. The live app doesn't change until you       |
| publish.                                             |
|   bindings:                                          |
|     tracker: { node: ../Kalamaja/tracker, role: edit }|
| The preview uses a masked copy of tracker: personal  |
| values are stand-ins.                                |
|                                                      |
|                      [Cancel]  [Make a preview]      |
+------------------------------------------------------+
```

- The lines are shown in mono, exactly as written into `app.yaml`. The node path is relative
  when the target is inside the project, else from the root.
- For an archive source, the footer has "Copy lines" and "Close" and the text of journeys J17.

## A11. Issue page

```
Files > Projects > Garden planner > Issues > TypeError: plants is undefined
+--------------------------------------------------------------------------------+
| TypeError: plants is undefined   [Open]      [Fix with an agent v] [Mute] [...] |
| Found by Ruum · Garden planner                                                 |
+--------------------------------------------------------------------------------+
| 37 times · 12 people · first 14:05 · last 2 min ago · since version 12         |
| [ 24-hour bar chart ]                                                          |
| Where       src/PlantList.tsx:42                                               |
| Assignee    [Choose]                Status [Open v]                            |
| Linked      Preview Fix plant list (Ready to try)                              |
|                                                                                |
| LAST OCCURRENCES                                                               |
| 14:22  /plants  Firefox 133  [person 8f3a91]          [Open trace]             |
| 14:20  /plants  Safari 18    [person 2c71d0]          [Open trace]             |
| > Stack trace (source-mapped)                                                  |
|                                                                                |
| COMMENTS                                                                       |
| ...                                                                            |
+--------------------------------------------------------------------------------+
```

**A report issue** (`source: report`) shows, for Can edit, the part "What the reporter sent":
the description (untrusted text), the screenshot (a file preview), the details line, "*14*
records attached" with "Show records", and "The reporter chose" (per field: shared, stand-in,
removed). Under it: "Ask to see more" (people only): pick fields the reporter shared as stand-ins
or removed, a reason (at most 500 characters, "The reporter sees this."), "Send request".
Fields the reporter refused are not offered. "Original values are kept until *12 Dec*."

- "Fix with an agent" is APP-11 (step 3 menu; step 5 button).
- Issue fields from the template (steps, expected, severity) show as a definition list above
  the comments, editable with Can edit.
- The status menu: Open, In progress, Closed. "Back again" is set by Ruum only.

## A12. Share dialog with app roles

The step 1 share dialog (4.8) with the role menu of 4.36:

```
+--------------------------------------------------------+
| Share Project tracker                              [x] |
| [ Name or email                     ] [Can use     v]  |
|                                        IN RUUM         |
|                                        Can use         |
|                                        Can edit        |
|                                        Can manage      |
|                                        IN THE APP      |
|                                        approver        |
|                                          Can approve   |
|                                          hours         |
|                                        admin           |
|                                          Can change    |
|                                          settings ...  |
| PEOPLE WITH ACCESS                                     |
| Marko (you)            Can manage · admin              |
| Everyone in Studio Linn  Can use                       |
|   Everyone who can use the app is a member:            |
|   Can log hours.                                       |
| Kadri                  Can use · approver   [...]      |
| Helena (guest)         Can use              [...]      |
+--------------------------------------------------------+
```

- App role names and descriptions are the app's text (untrusted): plain text, marked "From the
  app" for screen readers and in a tooltip.
- A row with only an app role reads "approver" (the role includes using the app).

## A13. Report page

`/report` on the Ruum address, a top-level window, the reduced shell (Ruum logo, no rail).

```
+--------------------------------------------------------------------------------+
| [Ruum]                                                                         |
| Report a problem with Project tracker                                          |
| Project tracker runs on Ruum. Your report goes to the people who made it.      |
|                                                                                |
| 1  WHAT HAPPENED                                                               |
| What were you doing, and what went wrong?                                      |
| [ I saved my hours for Monday and the page went blank.                    ]    |
|                                                                                |
| 2  WHAT RUUM ATTACHES                                                          |
| You see everything before it is sent. Remove what you don't want to share.     |
| +----------------------------------------------------------------------------+ |
| | Screenshot  [ thumbnail ]                       [Draw or blur] [Remove]    | |
| | Details     Page /hours · Firefox 133 · 1280 x 800 · version 7  [Remove]   | |
| | Records of this visit                                            [Remove]  | |
| |   Ruum found 14 records of your visit. 3 of them hold personal details.    | |
| |   Do this for all: [Use a stand-in v]                                      | |
| |   email   helena@example.com  (Share as it is)(Use a stand-in)(Remove)    | |
| |   name    Helena Kuusk        (Share as it is)(Use a stand-in)(Remove)    | |
| |   phone   +372 5555 1234      (Share as it is)(Use a stand-in)(Remove)    | |
| |   Only records of your own visit. Nobody else's details are attached.      | |
| +----------------------------------------------------------------------------+ |
|                                                                                |
| 3  SEND                                                                        |
| Email for an answer (optional)                                                 |
| [ name@example.com                     ]                                       |
| Only the team sees it. Ruum deletes it 30 days after the report is closed.    |
|                                                                [Send report]   |
+--------------------------------------------------------------------------------+
```

- Parts are 4.38. A removed item collapses to one line "Screenshot removed. [Put it back]".
- The page shows the reporter's own values only when the app keeps original values; else the
  value column says "(not kept)" and "Share as it is" is disabled with its reason.
- `frame-ancestors 'none'`; the page never runs app code; the app's name is text.

## A14. Receipt page

`/r#<token>` on the Ruum address, the reduced shell, no sign-in.

```
+--------------------------------------------------------------------------------+
| [Ruum]                                                                         |
| Your report: Hours didn't save                                                 |
| To the team of Project tracker · sent 10 Nov, 14:25                            |
|                                                                                |
| Status   [Being worked on]                                                     |
|          Received  >  Being worked on  >  Closed                               |
|                                                                                |
| THE TEAM ASKS                                                                  |
| Project tracker's team asks to see: email.                                     |
| Their reason (written by the team): We can't find your account without it.     |
| email   helena@example.com                       [Don't allow]  [Allow]        |
|                                                                                |
| WHAT YOU SHARED                                                                |
| Screenshot, details, 14 records                                                |
| email: stand-in · name: removed · phone: stand-in               [Change]       |
| You can change your choices until 12 Dec. Then Ruum deletes the original      |
| values.                                                                        |
+--------------------------------------------------------------------------------+
```

- Status words: copy.md 3.9. `regressed` shows as "Being worked on".
- The team's reason is untrusted text (plain text, quoted, "written by the team").
- "Change" opens the per-field choice list (4.38) for the fields of the report; "Share as it
  is" is possible only while original values exist.
- The token stays in the fragment; the page never puts it in a request URL (the API takes it in
  the body).

## A15. Settings

Sections in this order, each with its own "Save" (Can manage; read only for Can edit):

```
| WHO CAN USE IT                                                                 |
| (o) People you share it with (they sign in)                                    |
| ( ) Anyone on the internet                                                     |
|     Ruum's team must turn this on for Studio Linn first.                       |
|                                                                                |
| SECRETS                                                                        |
| STRIPE_KEY      Entered by a person  No value yet        [Enter value]         |
|   Stripe secret key of the shop account (from app.yaml)                        |
| SESSION_KEY     Made by Ruum  Rotated 3 Nov · every 90 days  [Rotate now]      |
|                                                                                |
| ERRORS AND PRIVACY                                                             |
| Browser errors:  (o) Privacy mode (recommended)  ( ) Full mode                 |
| [ ] Keep original personal values, encrypted, for [7] days                     |
| Log fields: 2 not confirmed. They count as Personal.     [Review log fields]   |
| [Copy the privacy notice paragraph]                                            |
|                                                                                |
| "REPORT A PROBLEM" BUTTON                                                      |
| [x] Show it in the live app   Place: [Bottom right v]                          |
|                                                                                |
| GO BACK BY ITSELF                                                              |
| [x] Go back to the previous version when errors go up after a publish          |
|     When errors are at least [3] times higher than before, and above 1%.       |
|                                                                                |
| ADDRESS                                                                        |
| tracker.linn.ruumapps.net                          [Copy]                      |
| Your own domain   Ruum's team must turn this on for Studio Linn first.         |
|                                                                                |
| API KEYS                                                                       |
| No API keys yet. Partners use them to call this app's API.     [New API key]   |
```

- "Enter value" opens a dialog: "Enter *STRIPE_KEY*", a password field with "Show while typing"
  (a toggle, off), the switch "Previews get this value" (off; helper "Turn on only for test
  keys. Previews run code that isn't published yet."), "Save value". The value is never shown
  again; the row says "Set by *Marko* on *10 Nov*". Agents see this row but cannot fill it.
- "Full mode" and raising "Keep original personal values" are for people only; the dialog says
  what Full mode records (copy.md 3.9).
- "Your own domain" with Ruum's approval: an "Add domain" field; the row then shows the DNS
  records to add, in mono with "Copy", and the state ("Waiting for DNS", "Getting a
  certificate", "Active", "Didn't work: *reason*").
- "New API key" (people only): name, API, scopes, rate per minute, ends in days; the key is shown
  once ("Copy the key now. Ruum shows it only once.").

## A16. Pages on the app's address

Served by the gateway on `<app>.<space>.ruumapps.net`, in Ruum's reduced look (logo, one
message, one action). Never the app's own look: a page of the gateway must not pass as the app.

| Page | When | Content |
|---|---|---|
| Preview bar | Every preview page (injected at the top, 32 px, closable for the page view) | "Preview of *app*. Only people who can edit it see this. Data: a masked copy. [Back to Ruum]" |
| Sign in | A platform app, not signed in | "Sign in to use *app*" with "Sign in with Ruum". |
| No access | Signed in, no `use` | "You don't have access to *app*. Ask the person who shared it with you." with "Go to Ruum". |
| Waking up | The app is asleep and takes more than 2 s | "Waking up *app*..." (a spinner; the page reloads when the app answers). |
| Not answering | The app crashed or doesn't answer (`502`, `504`) | "*app* isn't answering right now. Try again in a minute." with "Try again" and "Report a problem". |
| Paused | `suspended` | In the trash: "*app* is paused." Over a limit: "*app* is paused for now." Blocked: "*app* is blocked." Each: "If you use it, tell the person who shared it with you." |
| Preview ended | An expired or superseded preview | "This preview has ended. A newer one may exist; look in Ruum." with "Go to Ruum". |
| Not found | No app at this address | "There's no app at this address. Check the address, or ask the person who sent it." |
| Agent on a live app | An agent token on a live address (step 3) | `403`, no page; the hint in agent-experience 9.5. |
