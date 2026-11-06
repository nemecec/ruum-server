# UI copy

Status: `provisional`. Rules for the words in the web UI and emails, and the exact copy of the
main messages, errors and empty states. UI task cards copy these strings word for word. Words
for things follow `ia.md` (one word per concept). Agent-facing text (MCP, CLI, API hints) is in
`agent-experience.md`.

## 1. Voice

Calm, plain, a little warm. We are a careful colleague who keeps your work safe.

- **Plain English** (CLAUDE.md, "Writing"; ASD-STE100 in spirit): short sentences (at most 20
  words in instructions), active voice, present tense, common words.
- **Say what happened, then what to do.** "This link doesn't work any more. Ask the person who
  sent it for a new one."
- **Talk to the person** ("you", "your"). Ruum speaks of itself as "Ruum", not "we", except in
  the welcome card.
- **Contractions are fine** (don't, can't, you're): they read warmer and shorter.
- **Sentence case** in every title, button and label. No exclamation marks. No emoji.
- **No blame, no drama.** Not "You failed to...", not "Oops!", not "Whoops, something went
  wrong!". Not "Invalid", "Illegal", "Fatal", "Error 403".
- **No hype.** Not "magic", "smart", "AI-powered", "seamless", "effortless".
- **Be exact about safety.** Say "for good" only when it is for good. Say how to get things back
  ("You can restore it from the trash for 30 days").
- **Names over IDs.** Always the name of the item, person or agent. IDs appear only in "More
  facts" and error details.
- **Numbers.** Digits for all numbers ("3 items"); singular and plural forms are both written
  ("1 item", "2 items"); sizes as "2.4 MB"; dates as "5 Nov 2026" and times as "14:00" (24 h)
  in the person's locale; relative times under 7 days ("2 hours ago", "Yesterday at 14:00").
- **Names in text.** In these pages, *italics* mark a value that the UI fills in. The UI shows
  the name as plain text in medium weight, without quotes. Emails put names in quotes.

## 2. Patterns

| Pattern | Rule | Example |
|---|---|---|
| Button | Verb + object, at most 3 words | "Upload files", "Give access", "Restore this version" |
| Destructive button | The exact verb and object, with a count when more than one | "Delete 12 items for good" |
| Page title | A noun | "Trash", "Agents" |
| Confirm title | A question with the object | "Delete 12 items for good?" |
| Confirm body | What happens + how to get back (or that you can't) | "You can't undo this." |
| Toast | Past tense, what was done, with the name | "Moved plan.pdf to Site photos." |
| Empty state title | What the place is for, or that it is empty | "This folder is empty" |
| Error | What happened + what to do | "There's already an item called Plan here. Pick another name." |
| Helper text | One sentence, no period when it is a fragment | "At most your own access" |
| Placeholder | An example, never the label | "name@example.com" |
| Loading | "Loading *what*" for screen readers; visible skeletons, no text | "Loading items" |

## 3. Main messages

### 3.1 Sign in and onboarding

| Where | Copy |
|---|---|
| Sign in, title | Sign in to Ruum |
| Sign in, subtitle | Your files, with every version kept, and AI agents that work where you let them. |
| Buttons | Continue with Google / Continue with Microsoft / Sign in with a passkey |
| Under the buttons | New to Ruum? Signing in creates your personal space. |
| Signed out | You're signed out. |
| Cancelled | Sign-in was cancelled. Try again or pick another way to sign in. |
| Provider failed | Sign-in didn't work. Try again, or pick another way to sign in. |
| Welcome card, title | Welcome to Ruum, *first name* |
| Welcome card, points | Upload files and folders, from any device. / Every change is kept as a version, so you can go back. / Connect an AI agent that works for you, only in the folders you choose. |
| Welcome card, buttons | Upload files / Connect an agent / Close |

### 3.2 Files

| Where | Copy |
|---|---|
| Toast, folder created | Folder *Name* created. |
| Toast, uploaded | Uploaded *name*. / Uploaded 12 files to *Folder*. |
| Toast, uploaded with failures | Uploaded 10 files. 2 didn't upload. [Show] |
| Toast, renamed | Renamed to *New name*. |
| Toast, moved | Moved *name* to *Folder*. [Undo] / Moved 3 items to *Folder*. [Undo] |
| Toast, trashed | Moved *name* to the trash. [Undo] |
| Toast, tag added | Added the tag *tag*. |
| Upload panel header | Uploading 4 of 12 / 12 uploads done / 2 uploads need you |
| Upload row status | Waiting / Checking / Uploading 45% / Finishing / Done / Paused, waiting for the connection / Didn't upload: *reason* / Cancelled |
| Upload conflict | *name* is already here. / Upload as a new version / Keep both / Skip / Do this for all 3 |
| Drop zone | Drop to upload to *Folder* |
| Drop zone, view only | You can't add files here |
| New folder dialog | Title: New folder. Label: Folder name. Button: Create |
| Rename dialog | Title: Rename *name*. Label: Name. Button: Rename |
| Move dialog | Title: Move *name* / Move 3 items. Button: Move here. Disabled reason: "It's already here." / "A folder can't go inside itself." |

### 3.3 Sharing

| Where | Copy |
|---|---|
| Dialog title | Share *name* |
| Add field label / placeholder | Add people / Name or email |
| Role menu | Can view: see and download / Can comment: see and comment / Can edit: change, upload and rename / Can manage: also move, delete and share |
| Email option | Send an email / Note (optional) |
| Button | Share |
| Toast | Shared with *name or email*. / Changed *name* to can edit. / Removed access for *name*. [Undo] |
| List heading | People with access |
| Only me | Only you can see this. |
| Inherited row | from folder *Folder* |
| Pending email | Invited, not joined yet |
| Everyone row | Everyone in *Space* |
| Cannot share | You can see who has access, but you can't share this. Ask *owner name*. |
| Link heading / switch | Link / Anyone with the link |
| Link fields | Access / Ends on / Password (optional) |
| Link created | Copy the link now. For safety, Ruum shows it only once. [Copy link] |
| Link copied | Link copied. |
| Link row | Can view, ends 13 Nov 2026, with password |
| Turn off link, confirm | Title: Turn off this link? Body: People who use it lose access at once. You can make a new link later. Button: Turn off link |
| Links off | Links are turned off in this space. |
| Setting | People who can edit can also share / Yes / No / Same as the folder above (now: yes) |
| Visitor, note | You're viewing as anyone with the link. You can view. |
| Visitor, password | This link has a password. / Password / Open |
| Visitor, wrong password | That password isn't right. Try again. |
| Visitor, link ended | This link doesn't work any more. Ask the person who sent it for a new one. |

### 3.4 Versions

| Where | Copy |
|---|---|
| Tab | Versions |
| Current badge | Current |
| Restored row | Restored from 5 Nov, 14:00 |
| Retention note | Versions older than 30 days are removed on your plan. Named versions are kept forever. |
| Compare header | Compare versions / From / To / Compare as |
| Summary | 3 added, 1 removed, 2 changed |
| Same | These versions are the same. |
| Cannot compare | These files can't be compared line by line. [Download both] |
| Truncated | Only the first 500 changes are shown. Download both versions to see all. |
| Restore, confirm | Title: Restore the version of 5 Nov, 14:00? Body: It becomes the current version. The version you have now stays in Versions. Button: Restore |
| Restore, toast | Restored. The version of 5 Nov, 14:00 is current again. [Undo] |
| Name dialog | Title: Name this version. Body: Named versions are kept forever, also when old versions are removed. Label: Version name. Placeholder: Sent to client. Button: Save name / Remove name |

### 3.5 Agents

| Where | Copy |
|---|---|
| Page intro | Agents work in Ruum for you, with the access you give them. Every change they make shows their name and yours. |
| Page intro, admin | As an admin, you see every agent in *Space*. |
| Connect card 1 | Connect an AI app / Claude, ChatGPT and other apps that use MCP. Add this server address in the app: [address] [Copy] |
| Connect card 2 | Use a key in a script or the CLI / Create an agent with its own key. |
| Agent by-line | *Agent* for *Person* |
| Status | Active / Paused / Access ended / Not used yet / Last used 2 minutes ago |
| Access summary | Can edit in *Clients/Pärn*, until 4 Feb 2027 / Can view everywhere you have access in *Space*, no end date |
| Access fields | Where / Only in a folder: [Choose folder] / Everywhere you have access in *Space* / Highest role / Ends on / Changes per hour, at most |
| Access helper | An agent never gets more access than you have. |
| Consent title | *App* wants to connect to Ruum |
| Consent warning | Ruum can't check the name or address of this app. Continue only if you started this. |
| Consent body | It will act as an agent for you, *Person*. It gets only the access you choose here. You can pause it at any time on the Agents page. |
| Consent buttons | Don't allow / Allow |
| Consent done | Connected. You can go back to *App*. |
| Consent missing | This connection request has ended. Start the connection again from the app. |
| New key | Title: New key for *Agent*. Label: Key name (optional). Placeholder: laptop. Button: Create key |
| Key shown | Copy the key now. Ruum shows it only once. [Copy] |
| Turn off key, confirm | Title: Turn off this key? Body: Programs that use it stop working at once. Button: Turn off key |
| Pause, confirm | Title: Pause *Agent*? Body: It can't read or change anything until you resume it. Its history stays. Button: Pause |
| Paused note | Paused. Its keys and connections don't work until you resume it. |
| Activity run | Today, 14:02 to 14:09, 7 changes [Undo these changes] |
| Activity verbs | Created / Renamed *old* to *new* / Moved to *Folder* / Copied / Moved to the trash / Restored from the trash / Deleted for good / Changed (new version) / Restored a version / Added the tag *tag* / Removed the tag *tag* / Gave access to *person* / Removed access for *person* |
| Undo, confirm | Title: Undo 7 changes by *Agent*? Body: These changes will be undone: [list]. These stay as they are: [list with reasons]. Button: Undo 7 changes |
| Undo reasons | someone changed it later / this can't be undone / you no longer have access |
| Undo, toast | Undid 7 changes. 1 stayed as it was. |

### 3.6 Space settings and invitations

| Where | Copy |
|---|---|
| Start a team | Title: Start a team space. Label: Team name. Body: You'll be the owner. You can invite people next. Button: Create team space |
| Team ready | *Team* is ready. Invite the people you work with. [Invite people] |
| Invite form | Emails / One or more, separated by commas / Role / Message (optional) / Send invitations |
| Member roles | Owner: everything, including billing and deleting the space / Admin: manage members, sharing rules and all agents / Member: work with everything in the team space / Guest: only what is shared with them |
| Invite toast | Invitation sent to *email*. / 3 invitations sent. |
| Invitation row | Invited, waiting. Ends 13 Nov 2026. [Cancel invitation] |
| Accept page | *Person* invited you to *Space*. / Sign in to join. / [Join *Space*] |
| Joined | You've joined *Space*. |
| Remove member, confirm | Title: Remove *Name* from *Space*? Body: They lose access to everything in this space at once. Their 2 agents are paused, their 3 links are turned off, and their app passwords stop working. Their files stay. Button: Remove *Name* |
| Last owner | A space needs at least one owner. |
| Sharing rules | Allow links / Allow links that can edit / Links end after at most (days) / Allow guests / Members can share |

### 3.7 Trash

| Where | Copy |
|---|---|
| Intro | Items stay in the trash for 30 days. Then Ruum deletes them for good. |
| Intro, admin | Admins can restore items for 60 days. |
| Restore toast | Restored *name* to *Folder*. [Show] |
| Delete for good, confirm | Title: Delete *name* for good? Body: You can't undo this. Its versions are deleted too. Button: Delete for good |
| Empty trash, confirm | Title: Delete 12 items for good? Body: Everything in the trash is deleted, with its versions. You can't undo this. Button: Delete 12 items for good |
| Empty toast | Deleted 12 items for good. |

## 4. Errors

The UI maps each API error `code` (`crates/ruum-core/src/error.rs`) to copy for people. The API's
`message` and `hint` are written for agents and developers; the UI does not show them as they
are. It shows them only inside the "Details" disclosure of an error note, together with the code
and the request ID.

| Code | Copy for people | Action |
|---|---|---|
| `bad_request`, `validation_failed` | Something in this form isn't right. Check the marked fields. (Field errors under the field.) | Focus the first field |
| `unauthenticated`, `token_expired` | You've been signed out. Sign in again to keep working. | Session dialog |
| `permission_denied` | You don't have access to do this. Ask *owner name* or an admin of *Space*. | Close |
| `scope_exceeded` | (agents only; never shown to people) | |
| `user_required` | (agents only; never shown to people) | |
| `not_found` | This item is not here any more. It may have been moved or deleted. | Go to Files |
| `conflict` | This changed while you were working on it. Refresh to see the newest version, then try again. | Refresh |
| `name_taken` | There's already an item called *Name* here. Pick another name. | Under the field |
| `version_mismatch`, `precondition_failed` | Someone else changed *name* a moment ago. Refresh to see their change. | Refresh |
| `idempotency_key_reused` | Ruum is still working on your last request. Wait a moment. | Retry later |
| `payload_too_large` | *name* is larger than *2 GB*, the largest file your plan allows. | Plan (admins) |
| `quota_exceeded` | *Space* is full. Free some space, or an admin can change the plan. | Plan (admins) |
| `plan_limit_reached` | You've reached the limit of your plan for *agents*. Remove one you don't use, or change the plan. | Plan (admins) |
| `rate_limited` | That was a lot at once. Wait a minute and try again. | Retry |
| `agent_disabled` | (agents only; for people: the agent's Paused state) | |
| `chunk_missing`, `hash_mismatch` | *name* didn't upload completely. Ruum will try again. | Auto retry once, then "Try again" |
| `cursor_expired`, `range_not_satisfiable` | (not shown; the UI recovers by reloading) | |
| `not_implemented` | This isn't available yet. | Close |
| `internal`, `unavailable`, network error | Ruum couldn't do this just now. Try again in a moment. If it keeps happening, contact support with the code below. | Try again |
| offline (no network) | You're offline. Changes wait until you're back. | Banner |

Rules: every error note has a way forward (a button or a clear instruction). The error note
appears where the problem is (field, row, region), not at the top of the page by default.
Never show a stack trace, an HTTP status number or an internal path to people.

## 5. Empty states

| Place | Title | Text | Action |
|---|---|---|---|
| Files (top), can edit | Your files go here | Upload files or make folders. Every change is kept as a version. | Upload files / New folder |
| Folder, can edit | This folder is empty | Drop files here, or upload them. | Upload files / New folder |
| Folder, view only | This folder is empty | | |
| Guest home | Shared with you in *Space* | You see only what people in *Space* shared with you. Nothing yet. | |
| Shared with me | Nothing shared with you yet | When someone shares a file or folder with you, it shows up here. | |
| Trash | The trash is empty | Items you move to the trash stay here for 30 days. | |
| Search, no results | Nothing called "*q*" | Check the spelling, or search for part of the name. | Search all of *Space* (when inside a folder) |
| Versions, one only | This is the only version | Ruum keeps a new version each time the file changes. | |
| Tags on an item | No tags | | Add a tag (can edit) |
| People with access | Only you can see this | | (the add field is above) |
| Links | Anyone with the link: off | | Turn on |
| Agents | No agents yet | Agents work in Ruum for you, with the access you give them. Every change they make shows their name and yours. | Connect an agent |
| Agent activity | *Agent* hasn't changed anything yet | Its changes show up here, grouped by run. | |
| Agent keys | No keys | Apps that connect with sign-in (OAuth) don't need a key. | New key |
| Members (only owner) | It's just you here | Invite the people you work with. | Invite people |
| Invitations | No open invitations | | Invite people |
| Activity log | No activity yet | Changes in *Space* show up here. | |
| App passwords | No app passwords | Make one to open your files in Finder or Explorer. | New app password |

## 6. Emails

Emails use the same voice. Subject lines say who and what.

| Email | Subject | Body (plain text) |
|---|---|---|
| Invitation to a space | *Person* invited you to *Space* on Ruum | *Person* invited you to the Ruum space "*Space*". Open this link to join: *link* (The link works for *n* days.) Message from *Person*: *message* If you didn't expect this email, you can ignore it. |
| Shared with you (email grant) | *Person* shared *Item* with you | *Person* shared "*Item*" with you. You can *comment*. Open it: *link* *Note* If you didn't expect this email, you can ignore it. |

The current invitation email (`crates/ruum-core/src/tenants/mail.rs`) matches this voice; only
the subject changes ("Invitation to *Space* on Ruum" becomes "*Person* invited you to *Space* on
Ruum") and the line about how long the link works is new (the server's invitation lifetime decides the
number).
