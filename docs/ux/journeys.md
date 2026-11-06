# Step 1 journeys

Status: `provisional`. The six journeys of step 1 (design 18.4.1). Each journey has the persona,
the steps with the screen of each step, the success criteria (the measures for the synthetic user
test, design 18.9), and the states. Names follow `ia.md`, components follow `design-system.md`,
and the strings in quotes are in `copy.md`.

Every journey must work:

- at desktop (1280 px) and phone (390 px) width, in light and dark mode;
- with the keyboard only, and with a screen reader (VoiceOver, NVDA);
- on a slow connection (Chrome "Slow 4G"), and say the truth when the connection drops.

## Common states

The states below apply to every screen. Each journey lists only what differs.

| State | Rule |
|---|---|
| Loading | Under 300 ms: show nothing new (no flicker). Over 300 ms: a skeleton in the shape of the content (rows, cards), with `aria-busy="true"` on the region and one polite live message "Loading *what*". Never a full-page spinner after the first load. Buttons that started an action show a spinner in the button and stay the same width. |
| Empty | An empty state (design-system.md): what this place is for, and the one next action as a button. Never only "No items." |
| Error | An inline error note at the top of the region that failed: what happened, and what to do. A "Try again" button when a retry can help. The rest of the page stays usable. Form errors sit under their field. |
| No permission | Do not show actions that the person cannot do. When a person reaches a page or item without access: "You don't have access to this" with the owner's name if the API gives it, and a way back. Never a raw `permission_denied` or `404` text. |
| Not found | Item gone or never existed: "This item is not here any more. It may have been moved, deleted or the link is wrong." with "Go to Files". |
| Offline | A thin banner under the top bar: "You're offline. Changes wait until you're back." Reading what is already loaded still works. Buttons that need the server are disabled with the reason in a tooltip and in `aria-describedby`. Uploads pause and resume by themselves (resumable uploads). When the connection is back, the banner says "Back online" for 3 s and the page refreshes its data. |
| Session ended | The API answers `unauthenticated`: keep the page, show a dialog "You've been signed out" with "Sign in again", which comes back to the same URL. Unsent form input stays. |

## J1. First sign-in and onboarding

**Persona:** Liis (also Jonas). **Goal:** reach her own Files and know the three things Ruum does
for her: keep files, keep every version, let agents help safely.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens ruum.space and clicks "Sign in" | Sign in | Three buttons: "Continue with Google", "Continue with Microsoft", "Sign in with a passkey". One line under them: "New to Ruum? Signing in creates your personal space." |
| 2 | Picks Google and signs in at Google | (identity provider) | Ruum opens the provider in the same tab. |
| 3 | Comes back to Ruum | Files, with the welcome card | The server made the personal space (`getMe`, `listTenants`). URL `/t/<space>/files?welcome=1`. |
| 4 | Reads the welcome card | Welcome card over the empty Files | Title "Welcome to Ruum, Liis". Three short points with icons (upload, versions, agents) and three buttons: "Upload files" (primary), "Connect an agent", "Close". The exact text is in copy.md 3.1. |
| 5 | Clicks "Upload files" | Files, upload picker | Leads into J2. The welcome card does not come back after it is closed (kept per person in the browser; the server state can come later). |

**Success criteria**

- Signed in and on Files within 60 s from the sign-in page (excluding the provider).
- The synthetic user can say in their own words what a version is and what an agent may do,
  after reading the card only.
- No unknown word (tenant, node, scope) on any screen of the journey.

**States**

| State | What the person sees |
|---|---|
| Loading | After the callback: the app shell with a skeleton list, not a blank page. |
| Empty | Files is empty: the welcome card, then the Files empty state (J2). |
| Error | Provider cancelled: back on Sign in with "Sign-in was cancelled. Try again or pick another way to sign in." Provider error: "Sign-in didn't work. Try again, or pick another way to sign in." |
| No permission | Not possible in this journey. A `return_to` to a page without access lands on that page's no-permission state. |
| Offline | Sign in page: "You're offline. Connect to the internet to sign in." The buttons are disabled. |
| Already signed in | `/signin` goes straight to Files. |

## J2. Upload and organise

**Persona:** Liis. **Goal:** put the drawings and photos of a new project into a folder, and tidy
names.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Clicks "New folder" | Files; dialog "New folder" | The field is empty with the placeholder "Folder name". "Create" is disabled until there is a name. |
| 2 | Types "Pärna 12" and presses Enter | Files | The new folder appears in place, with a short highlight. Toast "Folder Pärna 12 created." |
| 3 | Opens the folder, drags 12 files onto the list (or clicks "Upload") | Folder; drop zone; upload panel | While dragging: the whole list area shows "Drop to upload to Pärna 12". The upload panel opens at the bottom right with one row per file. |
| 4 | Waits; keeps working | Any page | The upload panel stays across pages. Its header says "Uploading 4 of 12" with an overall bar. Each file shows "Checking", "Uploading 45%", "Done" or an error. |
| 5 | One file has the same name as an existing file | Upload panel row | Ask once per batch: "plan.pdf is already here. Upload as a new version, keep both, or skip?" Default: "Upload as a new version" (the old one stays in Versions). |
| 6 | Renames a file | Item details; dialog "Rename" | The name without the extension is selected. Enter saves. |
| 7 | Moves three photos into "Site photos" | Select rows (checkboxes), "Move"; folder picker dialog | The picker starts in the current folder, shows breadcrumbs, and has "New folder" inside. "Move here" is the primary button. Toast "Moved 3 items to Site photos." with "Undo". |
| 8 | Adds the tag "client: Pärn" | Item details, Tags | A combobox: type to find a tag or "Create tag *client: Pärn*". |

**Success criteria**

- 12 files of 2-40 MB uploaded with no help; the person can tell at any time how many are done.
- A dropped connection during upload never shows "Done" for a file that is not saved; the upload
  resumes by itself when the connection returns.
- Move of several items in one action; undo works from the toast.
- Phone: upload from the camera roll works with "Upload" (no drag on touch).

**States**

| State | What the person sees |
|---|---|
| Loading | Folder: skeleton rows. Upload rows: "Waiting" before they start. |
| Empty | Folder empty and can edit: drop-zone empty state "This folder is empty. Drop files here, or upload them." with "Upload files" and "New folder". Empty and view only: "This folder is empty." without actions. |
| Error | Name taken: under the field, "There's already an item called *Name* here. Pick another name." Upload failed: in the row, "Didn't upload: *reason*." with "Try again". Storage full: panel header "Your space is full. Free some space or change your plan." with a link to Plan (admins) or "Ask an admin" (members). File too large: "*Name* is larger than *2 GB*, the largest file your plan allows." |
| No permission | View-only folder: no Upload, New folder, Rename, Move or Move to trash. Drag onto it shows "You can't add files here" and nothing uploads. |
| Offline | Upload rows show "Paused, waiting for the connection" and continue by themselves. New folder, Rename and Move are disabled with "You're offline". |

## J3. Share with a colleague

**Persona:** Liis shares with Helena (and a contractor). **Goal:** give the right person the right
access to one project folder, and know who has access.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | On "Pärna 12", clicks "Share" | Folder toolbar or item details; share dialog | Dialog title "Share Pärna 12". |
| 2 | Types "helena@" | Share dialog, "Add people" | A combobox suggests members and earlier guests of the space by name and email. Any valid email can be added. |
| 3 | Picks "Can comment", keeps "Send an email" on, adds a note, clicks "Share" | Share dialog | The role menu shows a one-line meaning for each role ("Can comment: see and comment, not change"). |
| 4 | Sees her in "People with access" | Share dialog | Row: name or email, "Invited, not joined yet" for an email without an account, role menu, "Remove access". Toast "Shared with helena@example.com." |
| 5 | Also makes a link for the contractor | Share dialog, "Link" section | "Anyone with the link" switch; when on: role (Can view or Can comment; Can edit only when the sharing rules allow it), "Ends on" date (default 7 days, at most the space maximum), optional password, "Copy link". |
| 6 | Closes the dialog | Folder | The folder row shows a "Shared" mark. |
| 7 | Helena opens the email and clicks the link | Accept invitation (`/invitations/accept`) | "Liis Tamm invited you to *Pärna 12* in Liis Tamm's space." Sign in buttons. After sign-in: she lands in the folder with a one-time note "You can comment here. Liis sees what you do." |
| 8 | The contractor opens the link | Open a link (`/s#token`) | No sign-in. The reduced shell with the folder content and a note "You're viewing as anyone with the link. You can view." Password links ask for the password first. |

**Success criteria**

- Liis shares a folder with an email in under 45 s and can answer "who has access?" from the same
  dialog.
- Every inherited row names the folder it comes from.
- Helena reaches the folder from the email in at most 3 actions (open link, sign in, done).
- A link visitor never sees a sign-up wall before the content.

**States**

| State | What the person sees |
|---|---|
| Loading | The dialog opens at once with the add field; the lists show skeleton rows. |
| Empty | "People with access": "Only you can see this." Link section: the switch is off: "Anyone with the link: off." |
| Error | Guests are off in the space: "This space doesn't allow sharing with people outside it. An admin can change this in Sharing rules." Links are off: the link section says "Links are turned off in this space." Role too high: never possible, because the menu shows only roles the person may give. Invalid email: under the field, "Enter an email address like name@example.com." Link ended or turned off (visitor): "This link doesn't work any more. Ask the person who sent it for a new one." Wrong link password: "That password isn't right. Try again." |
| No permission | Can see but not share: the dialog shows the lists read-only with "You can see who has access, but you can't share this. Ask *owner name*." Invitation for someone already a member: "You're already in *Space*." with "Open *Pärna 12*". |
| Offline | Add and link controls disabled with "You're offline". The lists stay visible. Link visitor offline: the browser error is out of our hands; when cached, the shell shows the offline banner. |

## J4. Find and restore an old version

**Persona:** Liis. **Goal:** the drawing from yesterday was better; get it back without losing
today's.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Finds the file (search or folder) and opens its details | Item details | The details show "Changed 2 hours ago by Claude for Liis" with the agent mark. |
| 2 | Clicks "Versions" | Versions tab in the details panel | One row per version: when (relative, absolute in a tooltip), who (person, or agent for person), size, and the version name if any. The current version is first and marked "Current". Days group the list ("Today", "Yesterday", "2 Nov 2026"). |
| 3 | Picks yesterday's version and clicks "Compare" | Version compare view | The compare view opens wide (a dialog at full width on desktop, a full page on the phone). Header: "Changes from 5 Nov, 14:00 to the current version". Summary "3 lines added, 1 removed, 2 changed". Text: a side-by-side or unified view (person's choice). Image: before and after with a slider. Other files: "These files can't be compared. Download both to compare them." |
| 4 | Clicks "Restore this version" | Confirm dialog | "Restore the version of 5 Nov, 14:00? It becomes the current version. The version you have now stays in Versions." Button "Restore". |
| 5 | Done | Versions | Toast "Restored. The version of 5 Nov, 14:00 is current again." with "Undo". A new current row shows "Restored from 5 Nov, 14:00". |
| 6 | Names the good version | Version row menu, "Name this version" | Dialog: "Named versions are kept forever, also when old versions are cleaned up." Field "Version name", for example "Sent to client". |

**Success criteria**

- From the file to a restored version in under 60 s, with no fear: the synthetic user says
  that nothing is lost before they click Restore.
- The person can tell which versions an agent made without opening each one.
- Compare shows the change for text, JSON and images; other files say so plainly.

**States**

| State | What the person sees |
|---|---|
| Loading | Versions: skeleton rows. Compare: "Comparing..." in the view, with the two version headers already shown. |
| Empty | A file with one version: "This is the only version. Ruum keeps a new version each time the file changes." |
| Error | Compare failed: "Ruum couldn't compare these versions. Download them to compare." Version cleaned up by the plan: the list ends with "Older versions are removed after *30 days* on your plan. Named versions are kept." Restore conflict (someone changed the file): "The file changed while you were looking. Refresh to see the newest versions, then restore again." |
| No permission | Can view: Versions, Compare and Download are there; Restore and "Name this version" are not. |
| Offline | The list stays; Compare, Restore and Download are disabled with "You're offline". |

## J5. Connect an AI agent and see what it did

**Persona:** Jonas (Claude Code over MCP), and Liis (Claude Desktop, set up with help).
**Goal:** connect an agent that can only touch one folder, let it work, then see and undo what
it did.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Opens Agents and clicks "Connect an agent" | Agents; Connect an agent | Two ways, as two cards: **"Connect an AI app"** (Claude, ChatGPT, any MCP app): the server address `https://ruum.space/mcp` with "Copy", and short steps for Claude Desktop, Claude Code (`claude mcp add --transport http ruum https://ruum.space/mcp`) and ChatGPT. **"Use a key in a script or the CLI"**: "Create an agent with a key". |
| 2 | Adds the server in Claude Code; Claude opens the browser | Connect *App* to Ruum (OAuth consent) | Title "Claude Code wants to connect to Ruum". A note in the warning style: "Ruum can't check the name or address of this app. Continue only if you started this." "It will act as an agent for you, Jonas Kask." |
| 3 | Chooses the access | Consent page, "Agent access" | Space (only when the person has more than one), "Where" (folder picker; default "Only in a folder you choose" with a "Choose folder" button; "Everywhere you have access" as the other choice), "Highest role" (default Can edit), "Ends on" (default 90 days), "Changes per hour, at most" (default 500, under "More options"). A plain summary under the fields: "Claude Code can view and change items in *Clients/Pärn*, until 4 Feb 2027." |
| 4 | Clicks "Allow" | Back in the app | The page says "Connected. You can go back to Claude Code." for the case when the redirect does not close the tab. |
| 5 | Asks Claude to write meeting notes | (Claude Code) | Every change is recorded with actor = the agent and on behalf of = Jonas. |
| 6 | Opens Files | Folder | Rows changed by the agent show the agent mark and "Claude Code for Jonas" in the Changed column. A filter chip "Changed by agents" above the list. |
| 7 | Opens Agents, then "Claude Code" | Agent page | The page: status ("Active, last used 2 minutes ago"), access summary with "Change access", keys (when any), and **Activity**: changes grouped by run (a run = changes with less than 10 minutes between them), each change as "Created *notes.md* in *Clients/Pärn*" with a link to the item. |
| 8 | Undoes one run | Activity, "Undo these changes" on a run | Dry run first. Dialog "Undo 7 changes by Claude Code?" lists what will be undone and what stays ("notes.md stays: Jonas changed it later"). Button "Undo 7 changes". Toast "Undid 7 changes. 1 stayed as it was." |
| 9 | Pauses the agent | Agent page, "Pause" | "Pause Claude Code? It can't read or change anything until you resume it. Its history stays." |

**Success criteria**

- Jonas connects Claude Code with a one-folder access in under 3 minutes, without reading docs
  outside the page.
- Liis, with the steps on the page, connects Claude Desktop in under 5 minutes, or knows exactly
  which step to ask her nephew about.
- From a changed file to "which agent did this, and for whom" in one look.
- Undo of one run without touching the person's own later changes.

**States**

| State | What the person sees |
|---|---|
| Loading | Agents: skeleton cards. Consent: the app name area shows a skeleton; the form is usable. |
| Empty | No agents: the empty state "Agents work in Ruum for you, with the access you give them. Every change they make shows their name and yours." Buttons "Connect an agent". No activity: "Claude Code hasn't changed anything yet." |
| Error | Agent limit reached: "You have *2* agents, the most your plan allows. Remove an agent you don't use, or change your plan." Consent request missing or expired: "This connection request has ended. Start the connection again from the app." Undo partly failed: the dialog lists each item that stayed and why, in plain words ("someone changed it later", "can't be undone", "you no longer have access"). |
| No permission | A member opens another person's agent: "Only the owner of this agent and space admins can see it." Admins see every agent of the space, with the owner's name on each card. |
| Offline | Connect page: the server address and steps stay visible; Allow is disabled with "You're offline". |

## J6. Invite a team member

**Persona:** Marko. **Goal:** bring a new architect into the studio's team space, with the right
role, and later remove an intern who left.

| # | Step | Screen | Notes |
|---|---|---|---|
| 1 | Has only a personal space; picks "Start a team space" in the space switcher | Dialog "Start a team space" | Field "Team name", for example "Studio Linn". Text: "You'll be the owner. You can invite people next." |
| 2 | Creates it | Space settings, Members | The new space is current. A banner "Studio Linn is ready. Invite the people you work with." with "Invite people". |
| 3 | Clicks "Invite people" | Invitations; invite form | Emails (several, separated by commas or new lines), role (Member, Admin, Guest), each with one line of meaning, optional message. |
| 4 | Sends | Invitations | Toast "Invitation sent to kadri@studiolinn.ee." The list shows "Invited, waiting" with the date it ends and "Send again" and "Cancel invitation". |
| 5 | Kadri accepts | Accept invitation | "Marko Saar invited you to Studio Linn." Sign in. Then she lands in the team Files with "You've joined Studio Linn." |
| 6 | Marko sees her as a member | Members | Row: name, email, role menu, "Joined 6 Nov 2026". |
| 7 | Removes the intern | Members, row menu "Remove from Studio Linn" | Confirm dialog that says the effect: "Remove Mart from Studio Linn? He loses access to everything in this space at once. His 2 agents are paused, his 3 links are turned off, and his app passwords stop working. His files stay." Button "Remove Mart". |

**Success criteria**

- A team space with 3 invitations sent in under 3 minutes.
- Marko can say what each role may do before he picks it.
- After removal, Marko can say what happened to the intern's agents and links, from the dialog
  alone.

**States**

| State | What the person sees |
|---|---|
| Loading | Members and Invitations: skeleton rows. |
| Empty | Invitations: "No open invitations." with "Invite people". Members (only the owner): "It's just you here. Invite the people you work with." |
| Error | Member limit: "Studio Linn has *6* members, the most your plan allows." Guests off and role Guest: under the role menu, "Guests are turned off in Sharing rules." Already a member: "kadri@studiolinn.ee is already in Studio Linn." Invitation ended (invitee): "This invitation has ended. Ask Marko Saar for a new one." Last owner: the role menu of the only owner is disabled with "A space needs at least one owner." |
| No permission | Members cannot open Space settings; the rail does not show it. A direct URL shows "Only admins of Studio Linn can change its settings." |
| Offline | Forms disabled with "You're offline"; lists stay visible. |

## Journey measures for the synthetic user test

| Journey | Task given to the persona agent | Pass when | Target (provisional) |
|---|---|---|---|
| J1 | "Sign in and find where your files go." | On Files, welcome card read | 100% success, at most 4 actions |
| J2 | "Make a folder for the Pärna 12 project and upload these 12 files into it. Put the 3 photos in a sub-folder." | All files in place | 90% success, no wrong "Done" |
| J3 | "Share the Pärna 12 folder with helena@example.com so she can comment. Then tell me who has access." | Grant exists, answer correct | 90% success, under 8 actions |
| J4 | "plan.pdf was better yesterday. Get yesterday's version back." | Old version current, new one in history | 90% success |
| J5 | "Connect this AI app so it can only change the Pärn folder. Then undo what it did today." | Scope = folder; run undone | 80% success (Liis), 95% (Jonas) |
| J6 | "Start a team space for your studio and invite kadri@studiolinn.ee as a member." | Invitation sent with role member | 90% success |
