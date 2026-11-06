# Gap analysis: the web UI on `main` against this design

Status: `provisional`, 2026-11-06. Method: a read of `web/src` on `main` (commit `c89bff7`), and
57 screenshots of the UI with the mock API (`docs/ux/tools/screenshots.mjs`, port 5183) at 1280 px
light and dark, and 390 px light. A heuristic pass (Nielsen's ten heuristics) and a check against
`journeys.md`, `ia.md`, `design-system.md` and `copy.md`. No axe run and no screen-reader run yet
(card UX-15 adds them to CI).

## What is already good

Keep these when the cards below change the screens:

- Native `<dialog>` with focus return, a skip link, landmarks, labelled icon buttons, `role`
  on status and error messages (`web/src/components/ui.tsx`, `web/src/app/Shell.tsx`).
- Agents everywhere carry "*agent* (agent for *person*)" text (`components/format.ts`
  `actorText`), the undo flow does a dry run before it acts, and the consent page warns that the
  app's name is unverified.
- The share dialog lists only roles the person may give (decision 0001-1), and says when the
  person can see but not share.
- Restore says "No version is lost". Empty trash says "You cannot undo this".
- Light and dark follow the system; the phone layout works without horizontal scroll at 390 px.

## Priorities

- **P0**: a step 1 journey cannot be finished. Fix before any polish.
- **P1**: the journey works but breaks trust, naming or accessibility rules.
- **P2**: polish and efficiency.

| ID | P | Card | Journey |
|---|---|---|---|
| UX-01 | P0 | Open a share link (`/s#token`) | J3 |
| UX-02 | P0 | Accept an invitation (`/invitations/accept`) | J3, J6 |
| UX-03 | P0 | Connect an agent page (MCP address and steps) | J5 |
| UX-04 | P1 | Design tokens and the new identity | all |
| UX-05 | P1 | Naming pass: space, access, version name, sharing rules | all |
| UX-06 | P1 | Error copy by code, not raw API messages | all |
| UX-07 | P1 | Empty states with a next step, and the welcome card | J1, J2, J5 |
| UX-08 | P1 | Upload panel across pages, drop zone, name conflicts | J2 |
| UX-09 | P1 | Offline banner and session-ended dialog | all |
| UX-10 | P1 | Agent mark and agent rows in lists and versions | J4, J5 |
| UX-11 | P1 | Destructive actions: styles and toasts with Undo | J2, J4 |
| UX-12 | P1 | File list: row selection, multi-select, sort, no "Details" button | J2 |
| UX-13 | P1 | Share dialog: people picker, inherited "from folder", link switch | J3 |
| UX-14 | P1 | Agent page: access summary, activity by run with links | J5 |
| UX-15 | P1 | Accessibility and visual checks in CI | all |
| UX-16 | P2 | Versions: day groups, relative times, compare view wide | J4 |
| UX-17 | P2 | App shell: left rail, space switcher menu, account menu | all |
| UX-18 | P2 | Space settings: names, member removal effects, invite many | J6 |
| UX-19 | P2 | Grid view for image folders | J2 |
| UX-20 | P2 | Agent-facing hints and CLI output | J5 (agents) |

Contract needs found on the way are at the end ("Contract needs"). They go to the architect
panel; no card may change `contracts/` by itself.

## Cards

The cards use the task card format of design 18.3. "May change" lists folders of area 8
(`web/`), except UX-20. Every card that changes visible strings updates the strings in
`web/src/test/flows/*.test.tsx` in the same commit (the tests check behaviour; the new copy comes
from `copy.md`). That is not weakening a test: the assertions stay, the expected text follows the
copy.

### UX-01 (P0): Open a share link

```
Goal:        A visitor opens a share link without an account and sees the shared item.
Evidence:    The server makes links as <public_url>/s#<token> (crates/ruum-api/src/routes/sharing.rs,
             share_link_url). web/src/app/routes.tsx has no /s route, and nothing in web/src calls
             resolveShareLink (POST /v1/share-links/resolve). Today every share link lands on
             "Page not found".
Contracts:   resolveShareLink; the share_link token type (bearerAuth); listChildren, getNode,
             getContentUrl with that token.
Design:      journeys.md J3 steps 8; ia.md "What a guest sees" (reduced shell); copy.md 3.3 (Visitor).
May change:  web/src/app/routes.tsx, web/src/pages/share/ (new), web/src/api/client.ts (a second
             client that sends the link token), web/src/mocks/, web/src/test/flows/.
Must not:    put the token in a URL query or in logs; store it beyond the tab (sessionStorage at most);
             show the owner's other items.
Done when:   a flow test opens /s#<token>, sees the folder, downloads a file; password links ask for
             the password; an ended link shows the "doesn't work any more" copy; the phone layout
             works; the token never appears in a request URL.
Out of scope: comments, edit links' upload (later card), sign-in to "save to my files".
```

### UX-02 (P0): Accept an invitation

```
Goal:        A person invited to a space (or sent an email grant) joins with one click after sign-in.
Evidence:    Invitation emails link to <public_url>/invitations/accept?token=
             (crates/ruum-core/src/tenants/memory.rs accept_url). No route or call for
             acceptInvitation in web/src. Invitees land on "Page not found".
Contracts:   acceptInvitation (user-only); loginStart with return_to.
Design:      journeys.md J3 step 7, J6 step 5; copy.md 3.6 (Accept page, Joined).
May change:  web/src/app/routes.tsx, web/src/pages/AcceptInvitation.tsx (new), web/src/mocks/,
             web/src/test/flows/.
Must not:    accept automatically without a click (the person must see which space they join).
Done when:   signed out -> sign-in -> back on the accept page -> "Join <Space>" -> lands in the
             space's Files with "You've joined <Space>."; ended and used tokens show their copy;
             a member opening their own invitation sees "You're already in <Space>."
Out of scope: resending invitations (contract need C-3).
```

### UX-03 (P0): Connect an agent page

```
Goal:        A person connects Claude, ChatGPT or Claude Code over MCP from the UI, with no other docs.
Evidence:    web/src/pages/agents/AgentsPage.tsx offers only "Register agent" (an agent with keys);
             the MCP address (<public_url>/mcp) is nowhere in the UI. The empty state says "connect
             an AI client with OAuth" without saying how. Journey J5 step 1 cannot start.
Contracts:   none new (the MCP address is the public URL + /mcp; the OAuth flow exists).
Design:      journeys.md J5 steps 1-4; copy.md 3.5 (Connect cards, Consent done).
May change:  web/src/pages/agents/, web/src/app/routes.tsx, web/src/pages/OAuthConsent.tsx,
             web/src/test/flows/.
Must not:    show or create a key on this page for the MCP path (OAuth needs no key).
Done when:   /t/<space>/agents/connect shows the address with Copy, steps for Claude Desktop, Claude
             Code (the claude mcp add command) and ChatGPT, and "Create an agent with a key";
             the consent page defaults to "Only in a folder you choose", ends on in 90 days, shows a
             one-line summary of the access, and shows "Connected. You can go back to <App>." when the
             tab stays open.
Out of scope: per-app screenshots, a CLI download page.
```

### UX-04 (P1): Design tokens and identity

```
Goal:        Replace the GitHub Primer palette and ad hoc values with the tokens of design-system.md.
Evidence:    web/src/index.css lines 3-37 copy Primer colours (#0b5cd5, #0d1117, ...); radius 6 px
             only; no spacing, type or motion tokens; h1 1.5rem everywhere.
Design:      design-system.md section 2 (colour, type, spacing, radius, motion); contrast verified by
             docs/ux/tools/contrast.mjs (all pairs pass).
May change:  web/src/index.css, web/src/components/.
Must not:    change behaviour or markup of pages (that is in other cards); add a CSS framework or a
             web font.
Done when:   every colour in index.css is a token; light and dark screenshots of the 19 main screens
             match the token table; prefers-reduced-motion removes transitions; axe shows no
             contrast failures (UX-15).
Out of scope: icons (UX-17), the agent mark (UX-10).
```

### UX-05 (P1): Naming pass

```
Goal:        One plain word per concept in every visible string (ia.md section 1).
Evidence:    "Tenant" in the switcher label and "Tenant not found" (web/src/app/Shell.tsx:34, :105),
             the consent page field "Tenant" (web/src/pages/OAuthConsent.tsx:136), "Everything I can
             access in this tenant" (web/src/pages/agents/ScopeFields.tsx:36), the breadcrumb root
             shows the space name instead of "Files" (web/src/pages/files/FilesPage.tsx:92),
             "Admin: <name>", "Policy", "Audit log" (web/src/pages/admin/AdminPages.tsx:25-31),
             "Label" for version names (web/src/pages/files/VersionsPanel.tsx), "Scope", "Highest role"
             and "Writes per hour", "Disable"/"Enable" for agents, "Revoke" for links and keys,
             "Kind" in details showing a MIME type.
Design:      ia.md section 1; copy.md sections 3 and 5.
May change:  web/src/ (strings only), web/src/test/flows/.
Must not:    rename routes without a redirect; change API calls.
Done when:   a grep of web/src/pages and web/src/app for the words in ia.md's "Never in the UI"
             column finds none in visible strings; flow tests pass with the new strings.
Out of scope: URL change /admin -> /settings (UX-17).
```

### UX-06 (P1): Error copy by code

```
Goal:        People see what happened and what to do, never the API's agent-facing message and hint.
Evidence:    web/src/api/client.ts:125 errorText() shows "<message> <hint>" as is. Hints are written for
             agents ("pick another name, or send on_conflict: rename", "see the request schema in
             contracts/openapi.yaml"), so people see API field names.
Design:      copy.md section 4 (table by code); design-system.md 4.14 (error note with "Details").
May change:  web/src/api/client.ts, web/src/components/, web/src/pages/ (call sites), web/src/test/.
Must not:    swallow the code and request ID: they stay in the "Details" disclosure.
Done when:   every ErrorCode of the contract maps to a copy.md string (a unit test iterates the
             contract's ErrorCode enum); name_taken shows under the name field; network failures show
             the "couldn't do this just now" copy with Try again.
Out of scope: offline detection (UX-09).
```

### UX-07 (P1): Empty states and welcome card

```
Goal:        Every empty place says what it is for and offers the next step; first sign-in explains Ruum.
Evidence:    "This folder is empty." without actions (web/src/pages/files/FilesPage.tsx:197),
             "No agents yet. Register one, or connect an AI client with OAuth.", "No tags.", "Only you.",
             "No links." as bare text; no onboarding after the first sign-in (J1 steps 3-5 missing).
Design:      copy.md sections 3.1 and 5; design-system.md 4.11; journeys.md J1.
May change:  web/src/components/ (EmptyState), web/src/pages/, web/src/test/flows/.
Must not:    show actions to people who cannot use them (view-only folder: no Upload).
Done when:   each row of copy.md section 5 is on its screen; the welcome card appears once on an empty
             personal space and does not come back after Close (kept in localStorage per user).
Out of scope: server-side onboarding state.
```

### UX-08 (P1): Upload panel, drop zone and conflicts

```
Goal:        Uploads stay visible and truthful across pages; drag and drop works; name conflicts ask once.
Evidence:    useUploads lives in FilesPage (web/src/pages/files/FilesPage.tsx:53), so the upload list
             is only on the Files page and goes away on Agents, Trash or Account; no onDrop handler
             anywhere in web/src; a file with a taken name fails with name_taken instead of asking.
Design:      design-system.md 4.5, 4.6; journeys.md J2 steps 3-5; copy.md 3.2.
May change:  web/src/pages/files/, web/src/app/Shell.tsx (mount point), web/src/upload/ (no protocol
             change), web/src/test/.
Must not:    change the chunking or hashing (web/src/upload/fastcdc.ts, chunker.ts).
Done when:   start 3 uploads, go to Agents: the panel is still there and finishes; drop files on the
             list and on a folder row; a conflict offers new version / keep both / skip; offline pauses
             and resumes; the leave-page warning shows while uploads run; live region updates are
             throttled (start, each 25%, errors, end).
Out of scope: folder upload (directories), background sync.
```

### UX-09 (P1): Offline banner and session dialog

```
Goal:        The UI tells the truth when the network or the session is gone, and keeps the work.
Evidence:    no use of navigator.onLine or online/offline events in web/src; an expired session shows
             the raw error text in whatever region failed.
Design:      journeys.md "Common states"; design-system.md 4.18; copy.md 4.
May change:  web/src/app/, web/src/api/client.ts, web/src/components/.
Done when:   with the network off, the banner shows, server buttons are disabled with the reason, and
             coming back reloads data; an unauthenticated answer opens the session dialog, and sign-in
             comes back to the same URL with form input kept.
```

### UX-10 (P1): Agent mark

```
Goal:        Anyone sees at a glance which changes an agent made, and for whom.
Evidence:    agents appear only as text "(agent for ...)" in a tooltip (title) of the Changed column
             (web/src/pages/files/FilesPage.tsx) and in muted text in versions; design 3.4 asks for
             "a small agent badge on versions and changes".
Design:      design-system.md 1, 2.1 (--color-agent), 2.6, 4.10.
May change:  web/src/components/ (AgentMark, Actor), web/src/pages/.
Done when:   the mark shows in the list's Changed column (visible, not only a tooltip), details,
             versions, agent activity and the audit log; screen readers hear "<agent>, an agent for
             <person>"; a filter chip "Changed by agents" filters the loaded rows.
Out of scope: server-side filtering by actor (contract need C-2).
```

### UX-11 (P1): Destructive actions

```
Goal:        Visual weight matches the risk; reversible actions are quick and undoable.
Evidence:    "Move to trash" is a filled red button in the details panel (screenshot
             phone-light-03-details): the rule `.actions button.danger:last-child` in
             web/src/index.css:168 fills every last danger button, and trash asks for confirmation
             although it can be undone. Notices are inline green text at the top of the page, often
             out of view.
Design:      design-system.md 4.12 (the table of confirm and button style), 4.13 (toasts), 4.16.
May change:  web/src/index.css, web/src/components/ (Toast region), web/src/pages/.
Done when:   Move to trash, Remove access and Move have no confirm and show a toast with Undo that
             works (restoreNode, addGrant, moveNode back); Delete for good, Empty trash, Turn off link
             or key and Remove member keep a confirm with a danger button that names the count.
```

### UX-12 (P1): File list interaction

```
Goal:        A file list that works like a file manager, with keyboard and multi-select.
Evidence:    each row has a "Details" button (web/src/pages/files/FilesPage.tsx:236), a file name is a
             button and a folder name a link, no selection, no sort, no bulk move or trash; on the
             phone only the name and "Details" show.
Design:      design-system.md 4.4; journeys.md J2 step 7.
May change:  web/src/pages/files/, web/src/test/flows/files.test.tsx.
Done when:   row click selects and opens details; checkboxes and Shift ranges; a selection bar with
             Move, Download, Move to trash; sort by Name, Changed, Size with aria-sort; folders first;
             natural sort; the keyboard path of J2 works.
Out of scope: grid (UX-19), drag to move.
```

### UX-13 (P1): Share dialog

```
Goal:        Liis shares in under 45 s and Marko can tell who has access and why.
Evidence:    web/src/pages/files/ShareDialog.tsx: email field only (no picker of members), inherited
             rows say "(from a parent folder)" without the folder, the link section is a form of three
             fields always open, link expiry is "Expires in days" as a number, roles have no meaning
             text, "Settings" offers "Inherit from the parent folder (yes now)".
Design:      design-system.md 4.8; journeys.md J3; copy.md 3.3.
May change:  web/src/pages/files/ShareDialog.tsx, web/src/test/flows/share.test.tsx.
Must not:    allow a role above the person's own (decision 0001-1).
Done when:   combobox suggests members (listMembers) by name; inherited rows name and link the folder
             (the grant's inherited_from); the link section is a switch with Ends on (date); role menu
             items have their one-line meaning; pending email rows show "Invited, not joined yet".
Out of scope: a "Shared" mark in the list (contract need C-1).
```

### UX-14 (P1): Agent page

```
Goal:        From an agent to "what did it do, and undo one run" in two clicks.
Evidence:    web/src/pages/agents/AgentPage.tsx: activity rows have no link to the item and no folder;
             no grouping; the scope form is always open as the main block; "Disable" in danger style
             at the top right.
Design:      design-system.md 4.10; journeys.md J5 steps 7-9; copy.md 3.5.
May change:  web/src/pages/agents/, web/src/test/flows/agents.test.tsx.
Done when:   access summary sentence with "Change access"; activity grouped by runs (gaps under 10 min)
             with "Undo these changes" per run (dry run first); rows link to the item; Pause / Resume as
             secondary buttons with the confirm copy.
```

### UX-15 (P1): Accessibility and visual checks in CI

```
Goal:        An agent's change cannot quietly break a screen (design 18.4.1).
Evidence:    web/ has no axe or visual snapshot tests; the flow tests run in jsdom only.
Design:      design-system.md 3 and 5.
May change:  web/e2e/ (new, Playwright, Chromium only), web/package.json (dev dependencies: a new
             dependency needs approval: @playwright/test, @axe-core/playwright), .github/workflows/ (the
             integrator's file; coordinate).
Done when:   for the 19 main screens (the list in docs/ux/tools/screenshots.mjs) at 1280 and 390 px,
             light and dark: axe shows no serious or critical violations, and snapshots are compared
             with a small threshold; keyboard paths of J2 and J3 pass.
```

### UX-16 (P2): Versions and compare

```
Goal:        Find the right old version fast, and compare it in a view large enough to read.
Evidence:    web/src/pages/files/VersionsPanel.tsx: absolute dates only, four buttons per row that
             wrap, the compare view renders inside the 360 px side panel (screenshot
             desktop-dark-05-diff), "Compare as" shows plugin ids ("text@1.0.0").
Design:      design-system.md 4.9; journeys.md J4; copy.md 3.4.
May change:  web/src/pages/files/VersionsPanel.tsx, web/src/components/format.ts.
Done when:   day groups, relative times with absolute in a tooltip, Compare + a More menu per row, the
             compare view wide (dialog or page), plugin names in plain words, the retention note.
```

### UX-17 (P2): App shell

```
Goal:        The shell of ia.md section 2: rail, space switcher menu, account menu, phone bottom bar.
Evidence:    web/src/app/Shell.tsx uses a native <select> for spaces, a text link with the user's name,
             a "Menu" toggle on the phone that opens the whole nav, "Admin" in the main nav.
Design:      ia.md section 2; design-system.md 4.1, 4.2.
May change:  web/src/app/, web/src/index.css, web/src/app/routes.tsx (/admin -> /settings with redirect).
Done when:   desktop rail and phone bottom bar; switcher shows space kind and role; Space settings only
             for admins; focus moves to the h1 on route change.
```

### UX-18 (P2): Space settings

```
Goal:        Marko invites several people at once and knows the effect of removing one.
Evidence:    web/src/pages/admin/AdminPages.tsx: one email per invitation, roles without meaning, the
             remove confirm says "Their agents in this tenant are disabled." without links or app
             passwords; "Policy saved."
Design:      journeys.md J6; copy.md 3.6.
May change:  web/src/pages/admin/.
Done when:   several emails in one form (one createInvitation per email, with one summary toast); role
             meanings; the remove dialog lists the effects with counts where the API gives them;
             "Sharing rules" names.
```

### UX-19 (P2): Grid view

```
Goal:        Liis sees her site photos as pictures.
Design:      design-system.md 4.4 (grid); ia.md open questions.
May change:  web/src/pages/files/.
Done when:   a List/Grid toggle kept per folder; image thumbnails through getContentUrl, lazy loaded,
             with alt text = file name; keyboard and selection as in the list.
Out of scope: server thumbnails (later, media worker).
```

### UX-20 (P2): Agent-facing hints and CLI output

```
Goal:        Hints and CLI output follow agent-experience.md.
Evidence:    agent-experience.md section 5 (hints with REST paths and repository paths, a stale "still
             being built" hint) and section 6 (`ruum versions` prints agent:<uuid>, `ruum ls` prints raw
             RFC 3339, one exit code for all errors).
Contracts:   tool descriptions in contracts/mcp/tools.yaml are a contract change (C-4).
May change:  crates/ruum-cli/ (area 6); hint strings in their owning areas (one card per area).
Must not:    change error codes or the error body shape.
Done when:   no hint mentions contracts/ or a REST path when the caller is MCP; CLI tests for the new
             text output and exit codes; --json output unchanged.
```

## Contract needs

UX needs that the current contract does not cover (design 18.4.1: "UX needs can drive contract
changes before the contracts freeze"). For the architect panel.

| ID | Need | Why | Proposal |
|---|---|---|---|
| C-1 | Know if an item is shared, without one `listGrants` call per row | The "Shared" mark in the list (J3 step 6) | Add `shared: boolean` (has direct grants or live links) to `Node` |
| C-2 | Filter children and search by actor kind | "Changed by agents" filter over a whole folder (J5 step 6) | `actor_kind=agent` query parameter on `listChildren` and `searchNodes` |
| C-3 | Resend an invitation | "Send again" (J6 step 4) | `POST /v1/tenants/{id}/invitations/{inv}/resend` |
| C-4 | Better MCP tool descriptions and server instructions | agent-experience.md section 3 | A contract change of `contracts/mcp/tools.yaml` descriptions only (no names change) |
| C-5 | Owner's name in a no-permission answer | "Ask *owner name*" (journeys, no-permission state) | Optional `owner_display_name` in the `permission_denied` body for people who can see the item exists |
| C-6 | Copy a link again later | Liis loses the link after closing the dialog | Decide: keep "shown once" (safer; today's design) or store links encrypted; provisional answer: keep shown once, and make a new link easy |
