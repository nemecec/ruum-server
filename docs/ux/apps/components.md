# Components of step 3 (web apps, issues, templates, reports)

Status: `provisional`. New components 4.27 to 4.38, numbered after the step 2 components of
`../design-system.md` (4.19 to 4.26). On acceptance they move there as section "4c". They use
the tokens of section 2 only (Graphite where the brand applies); no new colour token is needed.
Rules for all of them:

- **Markers never look like data** (as 4b): stand-ins and hidden values in logs, occurrences and
  report records are the chips of 4.21.
- **The agent colour stays for agents**: the agent mark only where an agent did or asked
  something (made a preview, asked to publish, is the assignee).
- **App text is untrusted text.** Names, labels, role names and descriptions, step messages,
  log lines, issue titles and report descriptions come from code or people outside Ruum's
  control. They render as plain text: no markup, no links made from them, no icons chosen by
  them, long values truncated with the full text in a tooltip and in the accessible name.
- **Live is calm, preview is marked.** A preview is never shown without the word "Preview" or a
  "Ready to try" badge; a live address never has a preview mark.

### 4.27 App item and status badge

**In Files.** A row with the "app" outline icon (Lucide `app-window`), the name, the status badge
when the state is not Live, the agent mark when an agent made it, Changed. Quiet links in the
name cell: "*1* request", "*3* open issues", "*2* previews". A click on the name opens the app
page. The details panel of an app shows Status, Address, Live version, Who can use, Where, and
two buttons: "Open app" (primary for Can view) and "Open app page" (primary for Can edit).

**Status badge** (4.17 rules, text always present):

| `AppState` | Badge | Style |
|---|---|---|
| `provisioning` | Not live yet | neutral |
| `ready` | Live | success text; on the app page, not in Files rows |
| `updating` | Publishing | neutral, 12 px spinner (none with reduced motion) |
| `degraded` | Needs attention | warning |
| `suspended` | Paused | warning |
| `failed` | Didn't start | danger |

**Deployment badges** (in 4.29): `pending`, `running` "Getting ready" (with the current step
name after a colon), `ready` "Ready to try", `live` "Live", `failed` "Didn't start" (with the
step after a colon), `superseded` (no badge; the row is a plain earlier version), `expired`
"Ended", `rolled_back` "Taken back" (warning).

**Accessibility.** The row reads "Project tracker, app, Not live yet". The badge on the app page
is in a `role="status"` region; a change is announced once, politely.

### 4.28 Address

**Anatomy.** The address in `--font-mono`, `--text-sm`, without `https://`, truncated in the
middle on narrow screens ("tracker.linn.ruum…net"), followed by "Copy" (icon button with the
label "Copy address") and "Open app" (a link that opens a new tab, with the external-link icon
and the visually hidden text "(opens in a new tab)"). A preview's address has the prefix label
"Preview" in a neutral badge.

**States.** Default; copied (toast "Address copied."); no address yet ("The address appears with
the first preview."); your own domain active (that domain first, the default address under it in
muted text).

### 4.29 Release list (previews and versions)

**Preview row.** Label (or "Preview of *14:05*"), the badge (4.27), who made it (agent mark when
an agent) and when, the browser check in one line ("Check: no errors" / "Check: *2* errors"),
"Ends in *6* days" when under 3 days, and the actions "Open", "Publish" (primary text button) and
a menu.

**Live version row.** "Version *12*", the label, the badge when not plain (Live, Taken back),
"Published *14:20* by *Name*" or "(went back by *Name*)" / "(went back by Ruum)", and "Go back"
(not on the live row). The live row has a 3 px left edge in `--color-success`.

**Accessibility.** Each section is a list with a heading; each row is a list item whose accessible
name is "Version 12, Live, published 14:20 by Jonas". Actions have the version in their label
("Go back to version 11").

### 4.30 Step progress and browser check

**Step list.** A vertical list (a horizontal wrap on wide screens in A5) of the visible steps in
order (copy.md 3.9, "Steps"): each with a state icon (empty circle waiting, spinner running,
check done, cross failed, dash skipped), the step name, and for the running step an optional
second line (the step's message when it is a progress text from Ruum). `build` and `test` are
never shown (APP-4).

**Failed step.** The step opens: the people's copy of the hint (copy.md 3.9, "Step errors"), the
message (untrusted, mono when it quotes a file or a line), and "Details" with the stage name, the
error code and hint and `error.details` as formatted JSON with "Copy".

**Browser check.** One line with an icon: "Ruum opened *5* pages in a browser: no errors."
(success) or "Ruum opened *5* pages in a browser: *2* errors in the browser, *1* failed request."
(warning) with "Show" (a list of the check's `details`: route, kind, message as untrusted text)
and links to the new issues.

**Accessibility.** The list is an ordered list; the running step has `aria-current="step"`; each
state change is announced through one `role="status"` region ("Starting", "Ready to try", "Didn't
start: Checking app.yaml"). No announcement per tick.

### 4.31 Change summary and binding rows

**Change summary** (publish dialog, preview page). Groups in this order, each line with a sign
icon and words (never a colour alone): added (+), removed (−, warning style), changed (~):

- Access: "New access: *garden*, can change data" / "No longer reaches *Photos/Garden*" / "Can
  call *orders* of *Shop*: orders.read".
- App roles: "New app role: *approver*" / "Removes the app role *approver*. Its *2* holders keep
  the grant, and it works again if a version brings the role back."
- Tables: "Adds the table *favourites* with *3* columns, in *garden*" / "Changes *2* tables in
  *garden*" with "Show the change (SQL)".
- Log fields: "*2* new log fields" (the list follows in the classification part).
- Secrets: "*STRIPE_KEY* has no value yet." (danger).
- Nothing: "Nothing changes in access, roles or tables. Only the code is new."

**Binding row** (Access tab, preview page). The target's name (a link when visible), the kind
icon (database, folder, file, app), the access in words ("can read", "can change data", "can call
*api*: *scopes*"), who gave it and when, and "Remove..." (Can edit). In a preview: "(a masked
copy)" after a database's name.

**Accessibility.** Groups are lists with headings; the sign is also text for screen readers
("added", "removed").

### 4.32 Log viewer

**Anatomy.** A filter row (4.15 selects and a search field) and a list of records, newest first.
A record: time (`HH:MM:SS`, the date when not today), the level as a small badge with text
(Error danger, Warn warning, Info neutral, Debug faint), the origin (Server, Browser, Ruum), and
the message: the template in muted mono on the first line when the record has one, the message
with values on the second, with field values as chips: Normal values as plain mono text,
Personal as stand-in chips, Secret as hidden chips (4.21). Free text without a template carries a
small "scrubbed" note ("Free text: personal details were removed before storage.").

**Behaviour.** "Show older" pages back (`next_cursor`). New records appear at the top only when
the list is scrolled to the top; otherwise a pill "*12* new records" scrolls up on click (no
jumping list). The URL keeps the filters. A record opens the record panel.

**States.** Loading (skeleton rows); empty (copy.md 5); checking (a quiet line "Checking for new
records every 5 s"; "Paused while this tab is hidden"); offline (the line says "Offline: not
checking for new records").

**Accessibility.** The list is a `feed` (`role="feed"`, `aria-busy` while loading), each record an
`article` labelled "Error, Server, 14:22:05: order 1832 failed for person 8f3a91". New records are
not announced one by one; the pill is a button with its count.

### 4.33 Publish dialog and request card

**Request card** (Overview "Waiting for you", Agents page with C-24): as 4.24, with the line
"*Claude Code* for *Jonas* asks to publish *Favourites list*", the second line "Asked *3 min*
ago. Ends in *23 h 57 min*.", and "Review".

**Dialog** (screens.md A6): the title, the by-line for a request, the preview picture (C-31) with
"Open preview", the browser check (4.30), the change summary (4.31), the classification lists
(4.22) for new columns and new log fields, the effect lines (the version number, the restore
point, how to go back), and the footer. Publish mode: "Cancel" and "Publish". Request mode:
"Deny" (focused first) and "Approve and publish" or "Approve" (APP-6).

**Rules.** The dialog never publishes while it still loads the changes. Approving confirms every
class shown; the button says how many ("confirm *4* items") when there are any. A secret
without a value disables the primary button. The label and every name from the archive are
untrusted text.

**States.** Loading, ready, publishing (the step list of the publish replaces the body; "Close"
keeps it running), done (toast), failed (the failed step, "Nothing changed; version *12* is still
live."), ended, decided by someone else, read only.

**Accessibility.** A modal dialog (4.12); the preview link names its target; the classification
lists keep their 4.22 rules.

### 4.34 Traffic tiles

Three tiles (Visits, Errors, Speed) for the last 24 hours, from `getAppMetrics`: the number in
`--text-xl`, a sparkline (24 points, 1.5 px line in `--color-text-muted`, no fill), and a small
comparison with the day before in words ("Up 12% from yesterday"). Errors turn the number to
`--color-warning` text above 1% and add the word "high". Speed is the p95 in ms.

**Accessibility.** Each tile is a group with a heading; the sparkline is `aria-hidden`, and the
tile has a visually hidden sentence ("Errors: 0.4% of visits in the last 24 hours, down from 0.6%
the day before."). Follow the dataviz rules of the design system (no colour alone, both themes).

### 4.35 Issue row, status badge and issue facts

**Row.** The status badge, the title (untrusted, one line, truncated), the source in words, "since
version *n*", the last seen time, the assignee (person avatar or agent mark), the count "*37*
times" and "*12* people", and a 24-hour sparkline (`aria-hidden`). A group row: "*24* new errors
after version *13*" with "group".

**Status badge**: Open (neutral), In progress (info), Closed (success text), Back again (warning),
and "Muted until *17 Nov*" as a second quiet badge.

**Facts** (issue page): the counts line, a bar chart of the last 24 hours (with a data table
behind "Show as table"), "Where" (the top source-mapped frame in mono), Assignee, Status,
Linked (previews, sessions in step 5, the parent issue).

**Fix with an agent** (APP-11): a menu button. Step 3 items: "Copy a message for your agent",
"Assign to an agent". Step 5: the button becomes "Fix in a session" (primary) with the same menu
behind its arrow.

**Accessibility.** The row is a link to the issue with the accessible name "Open: TypeError:
plants is undefined, 37 times, 12 people, since version 12".

### 4.36 App roles in the share dialog

The role menu of 4.8 gets two groups with headings: "In Ruum" (Can use, Can edit, Can manage;
on an app, `view` is called "Can use") and "In the app" (each app role: the name in medium
weight, the description under it, both the app's text; the visually hidden text "From the app").
Inactive roles are not offered; existing grants of them show "Not in the live version" in the
list. A grantee can hold one Ruum access and several app roles; the list row shows them as
"Can use · approver · admin", and the row menu has "Remove *role*" for each.

A holder of a `grants_roles` role without `share` sees only the "In the app" group and the
intro "You can give app roles because you are an *admin* of *app*."

**Accessibility.** The menu is a listbox with groups (`role="group"` and `aria-label`); each app
role option's accessible name includes its description.

### 4.37 Template card

`--color-surface`, `--radius-lg`, an icon chosen by Ruum from the template kind (never an image
from the template in step 3), the name (h3), one line of what it is for, the "You get" line, the
source ("From Ruum" or "From *Space*") and "Choose". The whole card is one button; "Choose" is
its visible label end.

**Accessibility.** A list of buttons; each accessible name is "Internal tool: Lists and forms for
your team. You get: an app, a database, an approver role."

### 4.38 Report widget, report page parts and receipt parts

**"Report a problem" button** (on the app's address, drawn by the browser script in a closed
shadow root): a pill, 40 px high, the text "Report a problem" with a speech-bubble icon, in a
fixed corner (the setting), `--color-surface` with a 1 px border, readable on any background (it
carries its own background and focus ring), hidden while printing. The automatic prompt after an
error: a small card next to it, "Something went wrong. Tell the team what you were doing?" with
"Report a problem" and "Not now", shown at most once per page view.

**Report page parts** (A13): numbered sections with headings ("1 What happened", "2 What Ruum
attaches", "3 Send"). An attachment row: a label, the content (thumbnail, details line, records
summary), "Remove" (and "Put it back" after). The records part: the summary, "Do this for all"
(a select), and per personal field a row with the field name (mono), the reporter's own value
(or "(not kept)"), and a segmented control of three options ("Share as it is", "Use a stand-in",
"Remove"), default "Use a stand-in". The drawing layer: a full-window dialog with pen, box,
arrow, text, blur, undo and "Done" (Excalidraw, design 11.1).

**Receipt parts** (A14): a status stepper (Received, Being worked on, Closed) with the current
step marked by text and shape, not colour alone; a request card per reveal request (the fields,
the team's reason in a quote block marked "written by the team", "Don't allow" and "Allow" per
field); the shared choices list with "Change".

**Accessibility.** The segmented control is a radio group per field, labelled "email: what to
share"; the defaults are announced. The drawing layer has keyboard tools and a "Skip drawing"
button; blur works with the keyboard (move and resize a box with the arrow keys). The receipt
token is never in the page title or any request URL.
