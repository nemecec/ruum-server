# Step 3 journeys: web apps

Status: `provisional`. The seven journeys of step 3 (design 18.4.1, card U-3), numbered after the
eight of step 2 (`../databases/journeys.md`). Each journey has the persona, the steps with the
screen of each step, the success criteria (the measures for the synthetic user test, design
18.9) and the states. Names follow `README.md` (to move into `../ia.md`), components follow
`components.md` (4.27 to 4.38), screens follow `screens.md`, and the strings in quotes are in
`copy.md` (3.9).

The common states of `../journeys.md` (loading, empty, error, no permission, not found, offline,
session ended) apply to every screen. Each journey lists only what differs. Every journey must
work at 1280 px and 390 px, light and dark, with the keyboard only and with a screen reader. The
report page and the receipt page must also work for a person who has never seen Ruum.

The rules of the contract that shape these journeys (`docs/STEP3.md`, permission table):

- Using a live app needs `use` (Can view on the app, or any app role). Opening a preview, making
  previews, publishing and reading logs and errors need Can edit. Settings, secrets, domains and
  API keys need Can manage.
- Every binding needs the deployer to hold its role on the target; previews bind masked copies of
  databases and never touch live data.
- An agent's publish always waits for a person (a publish request); the person's one decision
  also confirms the new columns and log fields and the migrations on live data.
- Agents never enter secret values, never raise the telemetry mode, never make an app public,
  and do not use live apps in step 3 (only previews).

## J15. Create a web app from a template

**Personas:** Marko (team space, web UI) and Jonas (personal space, Claude Code over MCP).
**Goal:** a working app next to the project's files, with a preview to try, without writing code
and without knowing what a deployment is.

### J15a. Marko starts a project tracker (web UI)

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens "Projects" in Studio Linn, clicks "New", then "Project from template..." | Folder; "New" menu | The "New" menu: "Folder", "Database", "Project from template...", "Issue". Shown with Can edit on the folder. |
| 2 | Picks a template | New project from template (screens.md A1) | Template cards (4.37), "From Ruum" first: "Web app with a database and sign-in", "Static site", "Internal tool"; then "From *Studio Linn*" (the team's own). Each card: name, one line of what it is for, "You get" list (from the template manifest: "an app, a Postgres database, 4 issue kinds"). Search field above when there are more than 6. |
| 3 | Clicks "Internal tool", fills in the dialog | Template dialog (A1, second panel) | Label "Project name" (placeholder "Project tracker"), "Where": the current folder with "Change". When the template has role placeholders: "Who is the *approver*?" with the people picker of the share dialog (optional; "You can do this later."). The access line: "Everyone who can see *Projects* can see the new project. The app is open only to people you share it with." Button "Create project". |
| 4 | Waits | Setup progress (A2) | `instantiateTemplate` answers at once; the page polls `getTemplateSetup` with `wait=30`. A step list in plain words (copy.md 3.9: "Making folders", "Setting up the database *tracker*", "Adding sample data (masked)", "Making the first preview of *Project tracker*", "Adding issue kinds", "Giving access"). Each step: waiting, a spinner, done, or "didn't work" with the reason. |
| 5 | Setup is done | Setup progress | "*Project tracker* is ready to try!" with "Open the preview" (primary) and "Go to the project". When a placeholder stayed open: "Nobody is the *approver* yet. [Choose people]". |
| 6 | Opens the preview | The preview at its own address, in a new tab | The address is `<id>--tracker.linn.ruumapps.net`. A slim preview bar from the gateway at the top: "Preview of *Project tracker*. Only people who can edit it see this. Data: a masked copy." with "Back to Ruum". |
| 7 | Comes back and opens the app | App page, Overview (A3) | Status "Not live yet". "Next: try the preview, then publish it so people can use it." with "Open preview" and "Publish". |

### J15b. Jonas's Claude Code starts a garden planner (MCP)

| # | Step | Surface | Notes |
|---|---|---|---|
| 1 | Jonas types: "Start a garden planner web app in Projects with a database and sign-in." | Claude Code | |
| 2 | Claude calls `list_templates`, then `create_project_from_template` with `gallery:webapp-postgres` and the name "Garden planner" | MCP | Agent rules: `agent-experience.md` 9. |
| 3 | Claude calls `get_template_setup` with `wait=30` until `done` | MCP | No polling loop. |
| 4 | Claude tells Jonas: "Garden planner is set up: a database garden with sample data and a first preview. Try it here: (preview address). It is not live yet." | Claude Code | The preview opens for Jonas (Can edit) after sign-in. |
| 5 | Jonas opens "Projects/Garden planner" in Ruum | Folder | Rows: the app "Garden planner" with "Not live yet" and "Made by *Claude Code* for *Jonas*", the database `garden`, the folder "Issues", `AGENTS.md`, `project.yaml`, the source folder `app/`. |

**Success criteria**

- Marko gets from "New" to a preview he can click through in under 3 minutes, with no help.
- Marko can say, from the dialog alone, who will see the new project and who can use the app.
- Asked "is the tracker live?", Marko answers "no, it's a preview" and knows the next step.
- Jonas's agent finishes setup with at most 4 tool calls and no repeated call while it waits.
- No unknown word on any screen of the journey (deployment, manifest, instantiate, tenant,
  branch, staging).

**States**

| State | What the person sees |
|---|---|
| Loading | Template cards: skeleton cards; the dialog opens at once. |
| Empty | No templates from the space: the "From *Studio Linn*" group is not shown. The gallery is never empty. |
| Error | Name taken: under the field (`name_taken`). Plan full (`plan_limit_reached`): "You have *3* apps, the most your plan allows. Move one you don't use to the trash, or an admin can change the plan." A setup step that didn't work: that step shows "Didn't work: *reason*" and the page offers "Try again" (a new setup) and "Go to the project" (what was made stays); a database that waits for step 2 work (`database_branches_not_ready`): "The first preview waits for a feature Ruum is still building. The project and the database are ready." |
| No permission | Can view only: no "New" menu. Guest without the policy: "Project from template..." disabled with "Guests can't create projects in this space. An admin can turn this on in Sharing rules." |
| Offline | "Create project" disabled with "You're offline". A running setup keeps its steps and checks again when back online. |

## J16. See the status and logs of an app

**Personas:** Marko (status) and Jonas (logs). **Goal:** answer "is it working, are people
using it, is anything broken or waiting for me?" in one look, and find the log records behind a
problem in under a minute.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Sees the app in Files | Folder | The row (4.27): app icon, name, the badge when not Live ("Not live yet", "Publishing", "Needs attention", "Paused", "Didn't start"), quiet links "*1* request", "*3* open issues", "*2* previews". |
| 2 | Opens it | App page, Overview (A3) | Header: name (h1), status badge, the address (4.28) with "Copy" and "Open app", page actions "Share" and "Publish" (when a ready preview exists). |
| 3 | Reads the facts | Overview | "Live: version *12*, published *2 hours ago* by *Claude Code* for *Jonas*." "Who can use it: *8* people in Studio Linn, *2* guests." (C-25). "Asleep. It wakes on the next visit, in 1 to 3 seconds." (C-30). Traffic tiles (4.34) for the last 24 hours: visits, errors, speed (p95), with sparklines. |
| 4 | Reads what waits | Overview, "Waiting for you" | Publish request cards (4.33), the banner of the automatic rollback (APP-15), "Secrets missing: *STRIPE_KEY*. The next publish waits for it. [Enter value]" (manage only). Empty: the section is not shown. |
| 5 | Sees recent problems | Overview, "Issues" | The 3 newest open issues (4.35): title, "*37* times, *12* people", "since version *12*", with "See all issues". Empty: "No new issues in the last 7 days. Quiet is good." |
| 6 | Jonas opens Logs | Logs tab (A7) | The log viewer (4.32): newest first, the live app, last hour. Each row: time, level, Server/Browser/Ruum, the message with field chips. A filter row: "Live" / a preview, time ("Last hour", "Last 24 hours", "Since version *12*", a custom range), level, origin, words. |
| 7 | Filters to errors since the last publish | Logs | Level "Errors", time "Since version *12*". The URL keeps the filters, so Jonas can paste it to Claude or a colleague. |
| 8 | Opens one record | Logs, record details (a side panel; a sheet on the phone) | The message template in mono, each field with its privacy class and value (stand-in chips for Personal, hidden chips for Secret), the route, the version, and "Show the whole request" (all records with the same trace) and "Open trace" under "Details". |
| 9 | Looks up what happened to one customer | Logs, "Look up a person" (APP-14) | A small form: "Field" (a Personal log field, from `listAppTelemetryFields`), "Value" ("kati@example.com"). Helper: "Ruum turns the value into its stand-in and finds those records. It never stores what you type." Result: the filter "person *8f3a91*" is applied. |

**Success criteria**

- Marko can say whether the tracker works, how many people used it today and whether something
  waits for him, within 10 seconds of opening the page.
- Jonas finds the error records of the last publish and opens one in under 1 minute.
- Asked "what is the customer's email in this record?", the synthetic user says it is hidden
  behind a stand-in and never reads the chip as a value.
- Every state other than Live says what it means and what to do, in words.

**States**

| State | What the person sees |
|---|---|
| Loading | Skeleton header, facts and tiles; the tabs are usable at once. |
| Not live yet (`provisioning`) | "*Project tracker* isn't live yet. Try a preview, then publish it." with the newest preview's "Open preview" and "Publish". Traffic tiles are hidden. |
| Publishing (`updating`) | The badge with a spinner, and the step list of the publish (4.30) in the "Waiting for you" area. |
| Needs attention (`degraded`) | The reason in words (copy.md 3.9, status reasons), e.g. "More requests fail than usual since *14:20*." with "See issues". |
| Paused (`suspended`) | In the trash / over a hard limit / blocked by Ruum's team: copy.md 3.9 with the way out. |
| Didn't start (`failed`) | The reason in words, "See the steps" and "Try again" (publish the same version again). |
| Empty logs | "No log records in this time. Try a longer time, or check that the app writes to standard output." (with filters: "No records match these filters. [Clear filters]"). |
| No permission | Can view only: Overview without traffic, errors and requests; no Logs, Issues, Access or Settings tabs; a line "You can use *app*. People who can edit it see its logs and errors." |
| Offline | Facts stay; the status shows "Last checked *14:02*"; the log viewer stops checking for new records and says so. |

## J17. Give an app access to a database

**Persona:** Marko (Can manage on the `tracker` database of J7b and Can edit on the app).
**Goal:** the tracker app reads and changes the `tracker` database, safely, with a preview
first, and Marko knows what the app can now do.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens the app, tab "Access" | Access tab (A9) | Intro: "What *Project tracker* can reach. People who use the app don't get this access; the app does." List of what the live version reaches (4.31 rows): "*tracker-sample*: can change data" (the template's database). "Previews use a masked copy of each database." Button "Give access" (Can edit). |
| 2 | Clicks "Give access", picks "A database" | Give access dialog (A10) | Kinds: "A database", "A file or folder (read only)", "Another app's API" (this journey uses the first). The database picker shows databases Marko can see, nearest first. |
| 3 | Picks `tracker`, the access "Can change data" | Give access dialog | Choices: "Can read" (`view`) and "Can change data" (`edit`, databases only). "Name in the code" (the binding name, prefilled `tracker`, mono, helper "The app reads it from RUUM_BINDING_TRACKER."). The check line: "You can manage *tracker*, so you can give this." Or disabled: "You can only read *tracker*, so the app can get at most Can read." |
| 4 | Reads what happens | Give access dialog, summary | "Ruum adds these lines to app.yaml and makes a preview. The live app doesn't change until you publish." The 3 lines in mono. "The preview uses a masked copy of *tracker*: personal values are stand-ins." Button "Make a preview". |
| 5 | Clicks "Make a preview" | Preview page (A5) | `putContent` of `app.yaml` in the source folder (a new version, "Changed by *Marko*"), then `createAppDeployment` with `source.kind: folder` and the label "Access to tracker". The step list (4.30) runs: "Reading files", "Checking app.yaml", "Connecting data" ("a masked copy of *tracker*"), "Packaging", "Starting", "Trying it in a browser". |
| 6 | The preview is ready | Preview page | "Ready to try. Ruum opened 4 pages in a browser: no errors." with "Open preview" and "Publish". The access list of this preview shows "*tracker* (a masked copy): can change data". |
| 7 | Publishes | Publish dialog (A6, J18) | The change summary (4.31): "New access: *tracker*, can change data". Marko publishes. |
| 8 | Checks the database side | Database page, Overview of `tracker` | A new fact row (C-36): "Apps: *Project tracker* can change data." |

**When the source is an archive** (Jonas's CLI uploads `.ruum/garden.tar.gz`): steps 1 to 3 are
the same; step 4 says "This app comes from an archive, so Ruum can't change its app.yaml. Add
these lines, or ask your agent to:" with the lines and "Copy", and no "Make a preview".

**When an agent does it**: the agent edits `app.yaml` and calls `deploy_app_preview`; a binding
its user may not give fails at once (`binding_not_allowed`, the binding named in
`error.details.binding`); the agent tells its user which database and role it needs.

**Success criteria**

- Marko gives the app access to `tracker` and publishes it in under 3 minutes, without editing a
  file by hand.
- Asked "can people who use the app now change the database directly?", Marko answers "no, only
  through the app".
- Asked "did the preview change real data?", Marko answers "no, it used a masked copy".
- A person who may not give the access sees why before clicking, and who could.

**States**

| State | What the person sees |
|---|---|
| Loading | The database picker: skeleton rows. |
| Empty | No databases that Marko can see: "No databases here yet. Make one in a folder first. [New database]". The app reaches nothing yet: "*Project tracker* reaches no files, databases or other apps yet." |
| Error | Not allowed (`binding_not_allowed` at once): under the role choice, "You can only read *tracker*, so you can't give the app more. Ask *owner name*." Name in the code taken: under the field, "The app already uses the name *tracker*. Pick another name." A database preview that waits for step 2 work (`database_branches_not_ready`): "Previews with a database wait for a feature Ruum is still building. The change is saved in app.yaml." A step that didn't work: 4.30 failed state. |
| No permission | Can view: no "Give access"; the list is read only. Can edit without the right on the target: the role choices are capped, with the reason. |
| Offline | "Make a preview" disabled with "You're offline". |

## J18. Publish, approve a publish request, and go back

**Personas:** Marko (publishes his own preview), Liis (approves her Claude's request) and Jonas
(goes back to an earlier version; sees an automatic rollback). **Goal:** make a preview live
with a clear idea of what changes, and get back to the version before when something is wrong.

### J18a. Marko publishes his preview

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Clicks "Publish" on the preview (or the Overview) | Publish dialog (A6) | Title "Publish *Access to tracker*?". The preview's picture when C-31 lands, "Open preview". The browser check line (4.30). The change summary (4.31) against the live version: new access, removed access, new and removed app roles, tables that change (migrations) with their new columns, new log fields, secrets missing. |
| 2 | Reads the new columns | Publish dialog, "Who can see the new columns" | The classification review list (4.22, from `changes.planned_columns`), pre-filled with the author's classes; "Who can see the new log fields" the same for `unconfirmed_telemetry_fields`. Only shown when there are any. |
| 3 | Reads the effect | Publish dialog | "It becomes version *13*. People using *Project tracker* get it at once. You can go back to version *12* at any time." With migrations: "Ruum records a restore point of *tracker* first. The table changes stay if you go back to version 12." (forward-only migrations). |
| 4 | Clicks "Publish" | Publish progress (4.30 in the dialog, then the Overview) | Steps: "Updating tables", "Starting", "Trying it in a browser", "Going live". Toast "*Project tracker* is live! Version 13." with "Open app". |
| 5 | First publish only | Publish dialog | Two extra lines: "Who can use it: people you share it with. [Share after publishing]" and the telemetry choice (copy.md 3.9: "Privacy mode (recommended)" / "Full mode") with the privacy notice paragraph and "Copy" (design 9.6). |

### J18b. Liis approves her Claude's publish request

Claude Desktop made a preview of "Plant picker" with a new "Favourites" list and asked to
publish. The server answered `202` with a request; Claude told Liis in the chat: "I need your OK
to publish the new Plant picker. Here is the request: (review_url)".

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens the link | App page with the request dialog (A6, request mode; 4.33) | Title "Publish request from *Claude Desktop*". By-line with the agent mark: "*Claude Desktop* for *Liis* asked *3 minutes* ago. Ends in *23 h 57 min*." The label "Favourites list". "Open preview" (primary link, a new tab). |
| 2 | Opens the preview, comes back | Request dialog | The browser check: "Ruum opened 5 pages in a browser: no errors." The changes in plain words: "Adds the table *favourites* with *3* columns." "New log field: *plant_name*." |
| 3 | Checks the new columns | Request dialog | The classification review list: *client_email* suggested Personal ("looks like an email"), *plant_id* Normal, *added_at* Normal; *plant_name* (log field) Normal. Liis keeps the suggestions. |
| 4 | Clicks "Approve and publish" | Request dialog, then Overview | APP-6: decides `approve` with the classes and log fields, then publishes as Liis. Steps run; toast "*Plant picker* is live! Version 4." The request shows "Used". Claude's `get_app_approval` (waiting with `wait=30`) learns it (C-27) and tells Liis "It's live." |
| 4b | Or clicks "Deny" | Request dialog | An optional field "Tell *Claude Desktop* why (optional)" opens in place; "Deny" again sends. Toast "Denied. *Claude Desktop* was told." |

### J18c. Jonas goes back to an earlier version

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Version 13 shows a broken map; Jonas opens "Versions" | Versions tab (A4) | Two sections: "Previews" (ready previews, newest first) and "Live versions" (version 13 "Live", 12, 11 ...). Each live row: number, label, published when and by whom, "Go back" (Can edit). |
| 2 | Clicks "Go back" on version 12 | Go back dialog | Title "Go back to version 12?" Body: "Version 12 becomes live again. Version 13 stays in Versions; you can publish it again later." With a migration since 12: "The tables stay as version 13 changed them: Ruum doesn't undo table changes. Version 12 must work with them." and the button "Go back anyway" (`force`). |
| 3 | Confirms | Overview | Steps of the publish; toast "Version 12 is live again. [Undo]" (Undo publishes 13 again). |

### J18d. Ruum goes back by itself

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | After publishing version 14, the server errors rise | (no person) | The platform publishes version 13 again (`app.rolled_back`, actor the app) and opens an issue. |
| 2 | Jonas opens the app | Overview | The banner (APP-15): "Ruum went back to version *13* at *14:20*: errors went up after version *14*." with "See issues" and "Publish version 14 again". The Versions row of 14 has the badge "Taken back". The issue "*24* new errors after version *14*" is at the top of Issues. |

**Success criteria**

- Liis approves a request in under 2 minutes, opens the preview first, and can say in her words
  what changes ("a favourites list") and who sees client emails.
- Marko can say, before publishing, which version becomes live and how to go back.
- Jonas goes back to the previous version in under 30 seconds.
- After an automatic rollback, Jonas can say what happened and why, from the Overview alone.
- No person approves a request that names a missing secret without seeing that it is missing.

**States**

| State | What the person sees |
|---|---|
| Loading | The dialog opens with skeleton lines for the changes; "Publish" is disabled until they load. |
| Empty | No change against the live version: "Nothing changes in access, roles or tables. Only the code is new." |
| Error | Secret missing (`secret_missing`): the dialog says "*STRIPE_KEY* has no value yet. Enter it before you publish." with "Enter value" (manage) or "Ask *owner name* to enter it." "Publish" is disabled. Stale (`approval_stale`): "The preview changed after Claude Desktop asked. Ask it for a new request." A migration that failed: "The table change *0004_favourites* didn't work. Nothing changed; version *3* is still live." with "Details" (the statement number, the SQLSTATE). Not allowed to publish because a migration changes a database the person can't manage: "This change alters *garden*, which only people who can manage it may approve. Ask *owner name*." The check found problems: a warning line "The browser check found *2* errors." with "Show"; Publish is still possible (step 3 has no gate; step 4's tests block). |
| Request ended / decided | "This request has ended. *Claude Desktop* can ask again." / "*Name* already approved this at *14:05*." |
| No permission | Cannot decide: "Only *Liis* or people who can manage *Plant picker* can decide this." Can view: no "Publish", no "Go back". |
| Offline | Buttons disabled with "You're offline"; a running publish keeps its steps and checks again when back online. |

## J19. See errors and fix them in a session

**Persona:** Jonas. **Goal:** see that something broke, understand how bad it is, and hand it to
an agent with everything it needs, then see that the fix worked.

Step 3 has no agent sessions inside Ruum (design 11.1 is step 5). The step 3 path hands the
issue to an outside agent over MCP (APP-11); the step 5 path is noted where it differs.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Sees "*3* open issues" on the app row, opens the app, tab "Issues" | Issues tab (A8) | The issue list (4.35) for this app: open first, sorted by "Last seen". Each row: status, title (the error type and message), "*37* times, *12* people", a 24-hour sparkline, "since version *14*", "Found by Ruum" / "Reported by a user". Filters: Open / Back again / Closed / All; "Found by Ruum" / "Reported". |
| 2 | Opens the top issue | Issue page (A11) | Title "TypeError: plants is undefined". The facts: status, "Found by Ruum", "*37* times, *12* people, first *14:05*, last *2 minutes* ago, since version *14*". The trend chart. "Where": `src/PlantList.tsx:42` (source-mapped). The last occurrences (`listIssueEvents`): time, route, browser, person stand-in chip, "Open trace". Comments below. |
| 3 | Clicks "Fix with an agent" | Issue page, menu (APP-11) | Step 3: "Copy a message for your agent" and "Assign to an agent". Step 5: "Fix in a session" (opens a session with the issue as context). |
| 4 | Picks "Copy a message for your agent" | Toast | Copies: "Please fix the issue "TypeError: plants is undefined" in the app Garden planner in Ruum (issue *id*). Read it with get_issue and list_issue_events, find the cause, make a preview with the fix and check it with get_app_deployment, then ask me to publish." Toast "Copied. Paste it to your agent." |
| 4b | Or "Assign to an agent" | Assign menu | Lists Jonas's agents that can edit the app; picking one sets the assignee (`updateIssue`). The row shows the agent mark. Agents find issues assigned to them with `list_issues` (agent-experience 9). |
| 5 | Claude Code fixes it | Claude Code (MCP) | Reads the issue and events; edits files; `deploy_app_preview`; `get_app_deployment` with `wait=30` shows the check passed; sets the issue to In progress and links the preview (`update_issue`); `publish_app_deployment` answers `202`; Claude tells Jonas and gives the review_url. |
| 6 | Jonas approves and publishes | Request dialog (J18b) | The request shows "Fixes: *TypeError: plants is undefined*" when the issue links the preview. |
| 7 | The error stops | Issue page | After the publish, when no new occurrence comes for 1 hour, the issue offers: "No new occurrences since version *15*. [Close issue]" (design 9.7: "after the person confirms"). Closed issues that come back show "Back again" and move to the top. |
| 8 | Mutes a noisy issue | Issue page, "Mute" | "Mute for 7 days" (default), 1 day, 30 days. "Ruum keeps counting. A mute always ends." Row: "Muted until *17 Nov*". |

**Success criteria**

- Jonas finds the newest error and says how many people it hit, since which version, within 30
  seconds.
- Jonas hands the issue to his agent in under 1 minute, and the agent needs no second message to
  start.
- After the fix, Jonas can tell from the issue page that the error stopped.
- No log value appears in the issue list or the copied message (titles only; the agent reads the
  rest through tools that mark it untrusted).

**States**

| State | What the person sees |
|---|---|
| Loading | Issue list: skeleton rows. Issue page: skeleton facts; comments load after. |
| Empty | "No errors. Ruum checks every visit, and errors show up here." (with filters: "No issues match these filters. [Clear filters]"). |
| Grouped | One publish brought many new errors: one row "*24* new errors after version *14*" that opens the group (child issues listed on the parent's page). |
| Error | The issue page failed: the common note with "Try again". Assign failed (`permission_denied`): "*Claude Code* can't edit *Garden planner*. Change its access on the Agents page." |
| No permission | Can view: the issue page without occurrences and traces ("People who can edit *app* see where the error happens."), no "Fix with an agent". |
| Offline | Actions disabled with "You're offline". |

## J20. Share an app and give app roles

**Persona:** Marko. **Goal:** the studio can use the tracker; two people can approve hours;
Helena (guest) can use it too; Marko knows who can do what.

The tracker's `app.yaml` declares the roles `member` (default: "Can log hours"), `approver`
("Can approve hours") and `admin` ("Can change settings and give app roles", `grants_roles`).

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Clicks "Share" on the app page | Share dialog with app roles (A12; 4.36) | Title "Share *Project tracker*". The add field ("Name or email"). The role menu has two groups: "In Ruum" (Can use, Can edit, Can manage; "Can use" is `view` on an app) and "In the app" (the app's roles with their descriptions, marked "From the app"). |
| 2 | Adds "Everyone in Studio Linn" with "Can use" | Share dialog | Helper under the row: "Everyone who can use *Project tracker* is a *member*: Can log hours." |
| 3 | Adds Kadri and Tõnu with "approver" | Share dialog | The role menu lists "approver: Can approve hours". A person with an app role can open the app ("An app role includes using the app."). The toast "Kadri and Tõnu are now *approver* in *Project tracker*." |
| 4 | Adds Helena (guest) with "Can use" | Share dialog | The guest line: "Helena is a guest in Studio Linn. She can use the app; she sees nothing else here." |
| 5 | Reads who has what | Share dialog, "People with access" | Each row: the person, their Ruum access and their app roles as chips ("Can use · approver"). Group rows and "Everyone in Studio Linn". |
| 6 | Kadri should no longer approve | Share dialog | The row's menu: "Remove *approver*". Toast "Removed *approver* for Kadri. [Undo]". It takes effect at the next request in the app. |
| 7 | A version removes the role `approver` | Share dialog, App roles | The role shows "Not in the live version" (muted), its holders stay: "Grants stay, and work again if a version brings the role back." |

**Delegated role management**: Tõnu holds `admin` (`grants_roles`) but only `view` on the app
node. In the share dialog he sees only the "In the app" group and can give and remove app roles,
not Ruum access. The dialog says: "You can give app roles because you are an *admin* of
*Project tracker*."

**Success criteria**

- Marko gives two people the approver role in under 1 minute, in the dialog he already knows.
- Asked "can Helena see the studio's files now?", Marko answers "no, only the app".
- Asked "who can approve hours?", Marko answers from the dialog alone.
- No role name from the app looks like a Ruum role (the "From the app" mark and the group).

**States**

| State | What the person sees |
|---|---|
| Loading | The role menu's app group loads after the Ruum group; a skeleton line. |
| Empty | No app roles declared: the "In the app" group is not shown; the helper "This app has no roles of its own." under "Can use". No live version yet: "App roles come from the live version. Publish first." |
| Error | `permission_denied` (no `share`, not a holder of a role that gives roles): "You can see who has access, but you can't share this. Ask *owner name*." |
| No permission | Can view without share: the list is read only. |
| Offline | The add field disabled with "You're offline". |

## J21. Report a problem as an app user

**Persona:** Helena (a guest who uses Marko's tracker, signed in) and a client of Liis (a visitor
of a public app, no account; for the receipt link). **Goal:** tell the team what went wrong in a
minute, decide what of the visit she shares, and later see what happened to the report.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | The page breaks while Helena saves hours | The tracker, with the browser script's prompt | A small note from the browser script (bottom right, never over the app's own dialog): "Something went wrong. Tell the team what you were doing?" with "Report a problem" and "Not now". Without an error, the "Report a problem" button (4.38) sits at the bottom right. |
| 2 | Clicks "Report a problem" | The report page opens as a new window on the Ruum address (A13) | The handoff runs (gateway, then `/report/callback`). Title "Report a problem with *Project tracker*". Under it: "*Project tracker* runs on Ruum. Your report goes to the people who made it (*Studio Linn*, with C-35)." |
| 3 | Writes what happened | Report page, part 1 "What happened" | Label "What were you doing, and what went wrong?" (required, at most 5000 characters), placeholder "I saved my hours for Monday and the page went blank." With C-34 a second field "What did you expect?". |
| 4 | Reviews what is attached | Report page, part 2 "What Ruum attaches" | Each item is a row with "Remove" (4.38): **Screenshot** (C-28; else "Add a screenshot") with "Draw or blur" (opens the drawing layer: pen, box, arrow, text, blur); **Details**: "Page /hours, Firefox 133, 1280 × 800, version 7"; **Records of this visit**: "Ruum found *14* records of your visit. *3* of them hold personal details." |
| 5 | Decides the personal details | Report page, records list (4.38) | Per field, a segmented choice: "Share as it is", "Use a stand-in" (selected), "Remove". Each field shows Helena's own value when the app keeps original values ("email: helena@example.com"), else "Share as it is" is disabled with "*Project tracker* doesn't keep original values, so Ruum can only use a stand-in or remove it." "Do this for all: [Use a stand-in v]". A line: "Only records of your own visit. Nobody else's details are ever attached." |
| 6 | Leaves an email (optional) | Report page, part 3 "Send" | "Email for an answer (optional)". Helper: "Only the team sees it. Ruum deletes it 30 days after the report is closed." |
| 7 | Sends | Report page, done | Members and guests who can see the issue: "Thanks, your report is in. The team sees it in Ruum. [Open the report]". Others (Liis's client): "Thanks, your report is in. Keep this link to follow it. Ruum shows it only once." with the link and "Copy link"; "Close this window" returns to the app. |
| 8 | Follows the report later | Receipt page (A14), `/r#<token>` | "Your report: *Hours didn't save*". Status in words: "Received" / "Being worked on" / "Closed". "What you shared" (each choice, "Change"). "You can change your choices until *12 Dec*. Then Ruum deletes the original values." |
| 9 | The team asks to see more | Receipt page, "The team asks" | "*Project tracker*'s team asks to see: *email*." "Their reason (written by the team): *We can't find your account without it.*" Per field: "Allow" / "Don't allow". After "Don't allow": "The team can't ask about *email* again. You can still allow it here later." When she left an email, she got one (copy.md 6). |

**Success criteria**

- Helena files a report in under 2 minutes, and can say what she shared and what she didn't.
- Asked "can the team see your email address in the logs now?", Helena gives the answer that
  matches her choice.
- Liis's client keeps the link and later finds the status without an account.
- Nothing is sent before "Send report"; every item was visible and removable before it.
- The report page works in the reporter's language, with the keyboard only and with a screen
  reader, and never looks like part of the app (Ruum's header and address).

**States**

| State | What the person sees |
|---|---|
| Loading | The report page: the title at once, skeleton rows for the records. |
| Empty | No records with personal details: "Ruum found *14* records of your visit. None of them hold personal details." No records at all: "Ruum found no records of this visit." (the part stays, with nothing to choose). |
| Error | The handoff failed or the session ended (15 minutes): "This report page has ended. Go back to *app* and click Report a problem again." Too many reports (`429`): "That was a lot of reports at once. Try again in a few minutes." Screenshot too large (`413`): "The screenshot is larger than 5 MB. Make the window smaller, or leave it out." The description is empty: under the field, "Tell the team what happened." |
| Receipt link ended | The issue was deleted or the link is wrong: "This link doesn't work. The report may have been removed." |
| Original values gone | After the issue closed or 30 days: "Ruum deleted the original values of your visit. Stand-ins and removed values stay as they were." The request list shows "Ended". |
| No permission | Not signed in on a platform app: the gateway asks to sign in before the app opens, so the report page always has a session. |
| Offline | "Send report" disabled with "You're offline. Your text stays here." |
