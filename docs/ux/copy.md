# UI copy

Status: `provisional`. Rules for the words in the web UI and emails, and the exact copy of the
main messages, errors and empty states. UI task cards copy these strings word for word. Words
for things follow `ia.md` (one word per concept). Agent-facing text (MCP, CLI, API hints) is in
`agent-experience.md`.

Where the words live: the catalogue `web/src/locales/en.json` holds the English source of every
UI string, and `web/src/locales/context/en.json` holds each message's description, tone and length
budget for translators (`i18n.md`, "Context for translators"). The approved terms and the
do-not-translate list are in `glossary.md`. When this page and the catalogue differ, fix the one
that is wrong in the same change; the copywriter reviews every change that touches text (design
18.4).

## 1. Voice

**Informal and playful, never official or corporate** (decided by the person, 2026-11-08; for every
language). We are a friendly colleague with a sense of humour, who also keeps your work safe.

- **Informal address in every language** (Estonian "sina", German "du", French "tu", and so on).
- **Playful where nothing is at stake**: empty states, onboarding, success messages, small touches. A
  light joke or a friendly word is welcome there.
- **Calm and exact where something is at stake**: errors that can lose data, security, money and
  billing, legal texts, deleting for good. No jokes there; say what happened and what to do.

- **Which tone where.** Each message carries its tone in the context file: `playful` (empty
  states, welcome, success toasts, small nudges), `neutral` (labels, buttons, headings, lists) or
  `calm` (errors, security, sign-in, access, money and plans, deleting for good, legal). A playful
  line still says the facts: "Plenty of room here. Drop files in, or upload them." A calm line has
  no joke and no exclamation mark: "You can't undo this. Its versions are deleted too."

| Playful (nothing at stake) | Calm (something at stake) |
|---|---|
| Folder Invoices is ready for your files. | Delete plan.pdf for good? You can't undo this. |
| You're in! Welcome to Pärn Studio. | Copy the key now. Ruum shows it only once. |
| Claude Desktop is back at work. | Paused Claude Desktop. |
| Nothing to clean up. Items you move to the trash stay here for a while, so you can bring them back. | Items stay in the trash for 30 days. Then they are deleted for good. |
| Changed your mind? Ruum keeps every change as a version, so you can always go back. | The migration failed. Nothing changed. |

- **Plain English** (CLAUDE.md, "Writing"; ASD-STE100 in spirit): short sentences (at most 20
  words in instructions), active voice, present tense, common words.
- **Say what happened, then what to do.** "This link doesn't work any more. Ask the person who
  sent it for a new one."
- **Talk to the person** ("you", "your"). Ruum speaks of itself as "Ruum", not "we", except in
  the welcome card.
- **Contractions are fine** (don't, can't, you're): they read warmer and shorter.
- **Sentence case** in every title, button and label. An exclamation mark only for a real
  success, at most one per screen. No emoji (they translate and read badly with screen readers).
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
| Under the buttons | New here? Signing in makes your account and a personal space, all in one go. |
| Signed out | You're signed out. See you soon. |
| Cancelled | Sign-in was cancelled. Try again or pick another way to sign in. |
| Provider failed | Sign-in didn't work. Try again, or pick another way to sign in. Code: *code* |
| Welcome card, title | Welcome to Ruum, *first name* |
| Welcome card, points | Bring your files: upload them from any device, or drag them onto a folder on a computer. / Changed your mind? Ruum keeps every change as a version, so you can always go back. / Get a helper: connect an AI agent that works for you, only in the folders you choose. |
| Welcome card, buttons | Upload files / Connect an agent / Close |

### 3.2 Files

| Where | Copy |
|---|---|
| Toast, folder created | Folder *Name* is ready for your files. |
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
| Visitor, note | Anyone with this link can see this. You don't need an account. (comment: "see and comment on") |
| Visitor, password | This link has a password. / Password / Open |
| Visitor, wrong password | That password isn't right. Try again, or ask the person who sent you the link. |
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
| Same | These two are twins: nothing changed between them. |
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
| Connect card 1 | Connect an AI app / Works with Claude, ChatGPT and other AI apps that can add a connector. Add this server address in the app: [address] [Copy] |
| Connect card 2 | Use a key in a script or the CLI / Create an agent with its own key. |
| Agent by-line | *Agent* for *Person* |
| Status | Active / Paused / Access ended / Not used yet / Last used 2 minutes ago |
| Access summary | Can edit in *Clients/Pärn*, until 4 Feb 2027 / Can view everywhere its person has access in *Space*, no end date |
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
| Activity verbs | Created / Renamed *old* to *new* / Moved to *Folder* / Copied / Moved to the trash / Restored from the trash / Deleted for good / Saved a new version of *name* / Restored a version / Added the tag *tag* / Removed the tag *tag* / Gave access to *person* / Removed access for *person* |
| Undo, confirm | Title: Undo 7 changes by *Agent*? Body: These changes will be undone: [list]. These stay as they are: [list with reasons]. Button: Undo 7 changes |
| Undo reasons | someone changed it later / this can't be undone / you no longer have access |
| Undo, toast | Undid 7 changes. 1 stayed as it was. |

### 3.6 Space settings and invitations

| Where | Copy |
|---|---|
| Start a team | Title: Start a team space. Label: Team name. Body: You'll be the owner. You can invite people next. Button: Create team space |
| Team ready | *Team* is ready! Now invite the people you work with. [Invite people] |
| Invite form | Emails / One or more, separated by commas / Role / Message (optional) / Send invitations |
| Member roles | Owner: can do everything, also change the plan and delete the space / Admin: manages members, sharing rules and all agents / Member: sees everything in the team space; what they can change depends on their access / Guest: sees only what people share with them |
| Invite toast | Invitation on its way to *email*. / 3 invitations are on their way. |
| Invitation row | Invited, waiting. Ends 13 Nov 2026. [Cancel invitation] |
| Accept page | *Person* invited you to *Space*. / Sign in to join. / [Join *Space*] |
| Joined | You're in! Welcome to *Space*. |
| Remove member, confirm | Title: Remove *Name* from *Space*? Body: They lose access to everything in this space at once. Their 2 agents are paused, their 3 links are turned off, and their app passwords stop working. Their files stay. Button: Remove *Name* |
| Last owner | A space needs at least one owner. |
| Sharing rules | Allow links / Allow links that can edit / Longest a link works (days; leave empty for no limit) / Allow guests / Members can share |

### 3.7 Trash

| Where | Copy |
|---|---|
| Intro | Items stay in the trash for 30 days. Then Ruum deletes them for good. |
| Intro, admin | On some plans, admins can restore items for longer. |
| Restore toast | *name* is back in *Folder*. [Show] |
| Delete for good, confirm | Title: Delete *name* for good? Body: You can't undo this. Its versions are deleted too. Button: Delete for good |
| Empty trash, confirm | Title: Delete 12 items for good? Body: Everything in the trash is deleted, with its versions. You can't undo this. Button: Delete 12 items for good |
| Empty toast | Deleted 12 items for good. |

### 3.8 Databases (step 2)

Journeys J7 to J14 (`databases/journeys.md`), screens S1 to S10 (`databases/screens.md`).

**Create (J7, S1)**

| Where | Copy |
|---|---|
| New menu | Folder / Database |
| Disabled, guest | Guests can't create databases in this space. An admin can turn this on in Sharing rules. |
| Dialog | Title: New database. Label: Name. Placeholder: garden. Button: Create database |
| Kind of database | Postgres (recommended): For most apps. Tables, search, maps and vectors. / SQLite: One small file. For small apps with one writer. |
| More options | More options / Extensions |
| Extensions | postgis: Maps and places / vector: Search by meaning (embeddings) / pg_trgm: Search with typos / citext: Text that ignores upper and lower case / hstore: Key and value pairs / uuid-ossp: Make UUIDs / pgcrypto: Hashes and encryption in SQL / btree_gist: Ranges that must not overlap / unaccent: Search that ignores accents |
| Plan line | Plan: Shared small, up to *1 GB*, queries up to *30 s*. |
| Access line, personal | Everyone who can see *Folder* can read it. Agents read personal data as stand-ins. [See who has access] |
| Access line, team | Everyone who can see *Folder* can read it, also guests. People who can edit it can change data. [See who has access] |
| Setting up | Ruum is setting up *garden*. This takes a few seconds. |
| Ready toast | *garden* is ready. |
| Next steps | Next: run a query, or let an agent add tables. [Run a query] [Connect an agent] |
| Created by agent | Created by *Agent* for *Person* |
| Sharing rules switch | Guests can create databases / Their databases, branches and copies count against *Space*'s plan. |

**Status (J8, S2)**

| Where | Copy |
|---|---|
| Badges | Setting up / Ready / Updating / Restoring / Needs attention / Paused / Didn't start |
| Facts | Size: *120 MB* of *1 GB* / Backups: You can go back to any time since *30 Oct, 14:05*. / Your access / Created: *6 Nov 2026* by *name* |
| Your access | You can read *garden*. / You can read and change data. / You can read and change data, change the tables and see secret values. / Secret values are hidden from you. Only people who can manage *garden* see them. |
| Limits | Limits / Longest query: *30 s*. Rows per result: *10,000*. Largest result: *10 MB*. Largest database to branch: *1 GB*. |
| Updating | Ruum is applying new settings to *garden*. |
| Restoring | Ruum is restoring *garden* to *5 Nov, 14:00*. It answers no queries until this is done. |
| Paused, trash | *garden* is in the trash. It answers no queries. Restore it from the trash to use it again. |
| Paused, over limit | *garden* is over your plan's limit of *1 GB*. It answers no queries until you delete data to make it smaller or change your plan. |
| Didn't start | *garden* didn't start: *reason*. [Try again] [Move to trash] |
| Status reasons | `disk_quota_exceeded`: it is over the size your plan allows / `migration_failed`: the last migration failed, and nothing changed / `extension_not_available`: an extension in its settings isn't available / other: Ruum couldn't reach it. Ruum keeps trying. |
| Offline status | Last checked *14:02* |
| Row links in Files | *1* request / *3* columns to confirm / *2* branches |
| Unconfirmed banner | *3* columns are not confirmed. They count as Personal until you confirm them. [Review columns] |
| Copy a database, masked | The copy gets masked data: personal values become stand-ins and secrets are removed. |
| Copy toast | Copied *garden*. The copy has masked data. / Copied *garden* with all its data. |
| Settings history (tab) | Settings history / Restore these settings |
| Settings saved | Saved. Ruum is applying the new settings. |
| Kind fixed | Can't change after the database is made. |

**Query (J9, S3)**

| Where | Copy |
|---|---|
| Tab | Query |
| Mode | Read only / Can change data |
| Editor label / placeholder | Query / SELECT * FROM plants LIMIT 20 |
| Values | Values / Value for *$1* / Put values here, not in the query. Ruum never stores them. |
| Buttons | Run query / Run and change data / Running... / Stop |
| Editor help | Escape, then Tab, leaves the editor. |
| Footer | *12* rows · *34* ms · Read only / Changed *3* rows · *12* ms / *1* row |
| No rows | No rows match. |
| Cut | Showing the first *10,000* rows, the most your plan allows in one result. Add a WHERE or LIMIT to see the rest. / The result is larger than *10 MB*, the most your plan allows. Ruum shows the first *4,210* rows. |
| No tables | No tables yet. Tables come from migrations, or from a CREATE TABLE in Can change data. |
| Read only for viewers | You can read *garden*. Ask *owner name* to change data. |
| Chips | hidden: secret / hidden: personal / person *8f3a91* |
| Chip tooltips | Hidden. Only people who can manage *garden* see secret values. / Hidden from agents. / A stand-in for a personal value: *p_8f3a91c2d4e5*. The same value always gets the same stand-in. |
| Privacy badges | Personal / Secret / not confirmed |
| Masked branch note | This branch has masked data. Values like *person 8f3a91* are stand-ins, not real data. |
| Copy menu | Copy as CSV / Copy as table / Download CSV |
| Copy toast | Copied *12* rows. |
| Confirm, person's delete (DB-4) | Title: Delete every row of *plants*? Body: This query has no WHERE, so it changes every row. You can restore *garden* to the moment before, on the Backups tab. Button: Run and delete rows |
| Confirm, person's drop | Title: Remove the table *plants*? Body: The table and its rows go away. You can restore *garden* to the moment before, on the Backups tab. Button: Run and remove table |
| Confirm, row count (C-20) | About *1,240* rows. |

**Columns and classes (J10, S4, 4.22)**

| Where | Copy |
|---|---|
| Tab | Columns |
| Intro | Privacy classes decide what agents see. Normal: the real value. Personal: a stand-in, the same for the same value. Secret: hidden. |
| Class choices | Normal: Agents see the real value. For amounts, dates, plant names. / Personal: Agents see a stand-in like person 8f3a91. For names, emails, phones, addresses and notes. / Secret: Only people who can manage see it; agents see "hidden". For passwords, tokens and keys. |
| Filter | All columns / Only not confirmed (*3*) |
| Column state | confirmed / not confirmed / Personal until you confirm |
| Suggestion | Suggested by *Agent*: *reason*. / Suggested by Ruum: *reason*. / No suggestion. Personal until you choose. |
| Rule reasons | the name looks like an email column / a phone column / a name column / an address column / a password or token column / a notes column |
| Lowered warning | Agents will see the real values of *phone*. / Agents will see stand-ins for *token* instead of hiding it. |
| Reset by rename | Renamed from *mail* on *6 Nov*. Choose again. |
| Sample values | Show 3 example values / Ruum doesn't keep these values. |
| Buttons | Review *3* columns / Confirm *3* columns / Confirm 1 column |
| Read only | Ask *owner name* to confirm these. |
| All confirmed | All columns are confirmed. |
| Confirm toast | Confirmed *3* columns. Agents see them as you chose. |
| Changed meanwhile | The columns changed while you were looking. Review them again. |

**Requests (J10, J11, S5, 4.24)**

| Where | Copy |
|---|---|
| Section | Waiting for you |
| Card | *Agent* for *Person* wants to *what* in *table*. / About *1,240* rows. Asked *3 min* ago. Ends in *23 h 57 min*. [Review] |
| What, by reason | `drop`: remove the table *t* / `truncate`: empty the table *t* / `delete_without_where`: delete every row of *t* / `update_without_where`: change every row of *t* / `schema_change`: change the tables of *db* / `migration`: change the tables of *db* (migration *name*) |
| Dialog title | Review the change from *Agent* |
| Migration body | Adds the table *customers* with *4* columns. / Changes *2* tables. [Show the migration (SQL)] |
| Migration effect | Ruum records a restore point first. If the change fails, nothing changes. |
| Statement effect | If you approve, *Agent* can run exactly this query once, in the next 15 minutes. It still needs write access. |
| No count | Ruum can't count the rows for this change. |
| Columns heading | Who can see the new columns |
| Buttons | Deny / Approve / Approve and confirm *4* columns |
| Deny reason | Tell *Agent* why (optional) |
| Two-step (DB-10) | Approved. *Agent* can run the change in the next 15 minutes. Ruum will ask you to confirm the columns once it has run. / The change has run. Confirm the *4* new columns. [Confirm 4 columns] |
| Toasts | Approved. *Agent* can make the change now. / Denied. *Agent* was told. |
| Ended | This request has ended. *Agent* can ask again. |
| Decided by another | *Name* already approved this at *14:05*. / *Name* already denied this at *14:05*. |
| Cannot decide | Only *Person* or people who can manage *db* can decide this. |
| Status words | Waiting / Approved / Denied / Ended / Used |
| Migration failed | The change from *Agent* failed. Nothing changed. |

**Agent access (J11, S6, 4.23)**

| Where | Copy |
|---|---|
| Tab | Agent access |
| Intro | Agents read *garden*, with personal data as stand-ins and secrets hidden. Extra access is for a short time, and you can end it at any time. |
| Button | Give access |
| Prompt title | Give an agent access to *garden* |
| From a link | *Agent* asks for: *Change data*, *1 hour*. / Reason from *Agent* (Ruum didn't check this): *reason* |
| Fields | Agent / Access / For / Reason (optional) |
| Access choices | Change data: Insert, update and delete rows in *garden*. Queries that delete or replace data still ask you. / See personal data: See real names, emails and other personal values instead of stand-ins. Secret values stay hidden. |
| Durations | 15 minutes / 1 hour / 4 hours / 8 hours / 24 hours |
| Duration cap | *Agent* asked for *48 hours*. The most is 24 hours. / Personal data can be shown for at most 8 hours. |
| Summary, write | *Agent* can change data in *garden* until *15:42* today. Ruum records a restore point first, so you can go back to *14:42*. |
| Summary, personal | *Agent* can see personal data in *garden* until *15:42* today. Secret values stay hidden. |
| Give button | Give access for *1 hour* |
| Give toast | *Agent* can change data in *garden* until *15:42*. / *Agent* can see personal data in *garden* until *15:42*. |
| Agent not allowed | *Agent*'s access is Can view. Change its access on the Agents page first. / *Agent* is paused. Resume it first. / This link is for an agent of *Name*. Only *Name* can give it access. |
| Viewer cannot give write | You can only read *garden*, so you can't give write access. |
| No own agent | You have no agent that can reach *garden*. [Connect an agent] |
| Sections | Waiting for you / Access now / Earlier / Requests, decided |
| Grant row | Change data / See personal data / until *15:42* (*58 min* left) / Restore point *14:42* / Used *4* times |
| Ended row | Ended at *15:42* / Ended by *Name* at *15:10* / *Agent* gave it back at *15:10* |
| Row actions | End now / Go back to before this access |
| End toast | Ended write access for *Agent*. / Ended personal data access for *Agent*. |

**Branches (J12, S7)**

| Where | Copy |
|---|---|
| Section | Branches / New branch |
| Dialog | Title: New branch of *garden*. Intro: A branch is a copy of the database to try changes. Agents can change data in a masked branch without asking. Label: Name. Button: Make branch |
| Default name | *garden* branch *1* |
| Data choices | Masked copy (recommended): Personal values become stand-ins, secret values are removed. Personal columns that are not text become text. / Tables only: The tables and columns, no rows. / Full copy: Real data, also personal values. Agents need write access to change it. |
| From | From / Now / An earlier time / Pick a time after *30 Oct, 14:05*. |
| Full copy disabled | Only people who can manage *garden* can make a full copy. |
| Too large | *garden* is *2.3 GB*. Branches work for databases up to *1 GB*. Make a branch with tables only. |
| Banner | Branch of *garden* · masked copy · made by *Name* at *14:10*. Agents can change data here without asking. / Branch of *garden* · full copy · made by *Name* at *14:10*. Agents need write access to change data here. |
| Toast | Made the branch *garden branch 1*. |

**Backups and restore (J13, S8, 4.25)**

| Where | Copy |
|---|---|
| Tab | Backups |
| Window | You can go back to any time since *30 Oct, 11:02*. |
| No backup yet | Ruum makes the first backup within a few minutes. After that you can go back to any time in the last 7 days. |
| Buttons | Name this moment / Restore to an earlier time / Restore... |
| Point reasons | Before write access for *Agent* / Before migration *name* / Before a restore / (the label of a named point) |
| Points note | Restore points stay for 7 days, like the backups. |
| No points | No restore points yet. Ruum makes one before each migration and each write access. |
| Name dialog | Title: Name this moment. Label: Name. Placeholder: Before import. Button: Save name |
| Name toast | Named this moment *Before import*. |
| Picker title | Restore *tracker* to an earlier time |
| Go back to | Go back to / A time / A restore point / *Tallinn* time (UTC+2) / That's *20 minutes* before *Before import* (*11:20*). |
| Restore how | Restore how / As a new database next to *tracker* (recommended) / Replace the data in *tracker* |
| New name default | *tracker* (restored *6 Nov 11:00*) |
| In-place helper | *tracker* answers no queries while Ruum restores it, usually a few minutes. Ruum records a restore point of the data now, so you can undo this. |
| Buttons | Restore / Replace the data... |
| In-place confirm | Title: Replace the data in *tracker*? Body: The data goes back to *6 Nov, 11:00*. Changes after that are kept in the restore point Before a restore (now), so you can undo this. Button: Replace the data |
| Toasts | Restoring *tracker* to *6 Nov, 11:00* as a new database. [Show] / *tracker (restored 6 Nov 11:00)* is ready. / Restored *tracker* to *6 Nov, 11:00*. [Undo] |
| Failed | The restore didn't work. *tracker* is as it was. |
| Running | A restore of *tracker* is already running. |
| Outside window | Pick a time after *30 Oct, 11:02*. |
| Cannot restore | Only people who can manage *tracker* can restore it. |

**Query log (J14, S9, 4.26)**

| Where | Copy |
|---|---|
| Tab | Query log |
| Filters | Who: Everyone / People / *Agent* · What: All / Reads / Changes · Outcome: All / Failed |
| Note | Ruum never stores the values in queries. They show as value 1, value 2. |
| Mode badges | Read / Changed data |
| Outcomes | *12* rows / Changed *12* rows / Failed: *code in words* / Stopped: took too long / Cancelled / Waiting for you |
| Grant used | with write access / with personal data access |
| Value chips | value *1* |
| Details | The query / Values / None / Outcome / With write access until *15:42* / Approved by *Name* at *15:05* / Query ID |
| No access | Only people who can manage *garden* see the query log. |
| Agent activity line | Queries are in each database's Query log. |
| Agent activity verbs (journal, card C-2) | Got write access to *db* for *1 hour* / Got personal data access to *db* for *1 hour* / Gave back access to *db* / Applied the migration *name* in *db* / A migration failed in *db* / Made the branch *name* / Restored *db* to *time* |

## 4. Errors

The UI maps each API error `code` (`crates/ruum-core/src/error.rs`) to copy for people. The API's
`message` and `hint` are written for agents and developers; the UI does not show them as they
are. It shows them only inside the "Details" disclosure of an error note, together with the code
and the request ID.

| Code | Copy for people | Action |
|---|---|---|
| `bad_request` | Ruum couldn't do this. Try again, and if it keeps happening, contact support with the code under Details. | Details |
| `validation_failed` | Something in this form isn't right. Check the marked fields. (Field errors under the field; a list of addresses names the one Ruum refused: "*address* isn't an email address. Fix it, then send again.") | Focus the first field |
| `unauthenticated`, `token_expired` | You've been signed out. Sign in again to keep working. | Session dialog |
| `permission_denied` | You don't have access to do this. Ask *owner name* or an admin of *Space*. | Close |
| `scope_exceeded` | (agents only; fallback for people:) This agent doesn't have access to do this. You can change its access on the Agents page. | |
| `user_required` | (agents only; never shown to people) | |
| `not_found` | This item isn't here any more. Someone may have moved or deleted it. | Go to Files |
| `conflict` | This changed while you were working on it. Refresh to see the newest version, then try again. | Refresh |
| `name_taken` | There's already an item called *Name* here. Pick another name. | Under the field |
| `version_mismatch`, `precondition_failed` | Someone else changed *name* a moment ago. Refresh to see their change. | Refresh |
| `idempotency_key_reused` | Ruum is still working on your last request. Wait a moment. | Retry later |
| `payload_too_large` | *name* is larger than the largest file your plan allows. Make it smaller, or an admin can change the plan. (Name the size, "*2 GB*", once the UI knows it.) | Plan (admins) |
| `quota_exceeded` | *Space* is full. Empty the trash, or delete items for good, to make room. An admin can also change the plan. (Items in the trash still count; never "free some space": a space is a personal or team space.) | Plan (admins) |
| `plan_limit_reached` | You've reached the agent limit of your plan. Remove an agent you don't use, or an admin can change the plan. (Members: "... Remove a member, or ..."; another limit: "You've reached a limit of your plan. An admin can change the plan.") | Plan (admins) |
| `rate_limited` | That was a lot at once. Wait a minute and try again. | Retry |
| `agent_disabled` | (agents only; for people: the agent's Paused state) | |
| `chunk_missing`, `hash_mismatch` | *name* didn't upload completely. Ruum will try again. | Auto retry once, then "Try again" |
| `database_not_ready` | *garden* isn't ready yet. Ruum is still setting it up or restoring it. | Wait; the page checks again by itself |
| `query_failed` | The database couldn't run this query: *the sanitized message, in a sentence*. Check the query and try again. | Note under the editor; "Details" with the SQLSTATE |
| `read_only_query` | This query changes data. Turn on Can change data to run it. (Can view: "You can only read *garden*.") | Note under the editor |
| `statement_timeout` | The query ran longer than *30 s*, the most your plan allows. Narrow it with WHERE or LIMIT. | Note under the editor |
| `write_grant_required` | (agents only; for people, the agent's request shows in Agent access. Fallback: "This agent needs write access before it can change this database. Approve its request when it asks.") | |
| `migration_failed` | The migration *name* failed. Nothing changed. | "Details" with the statement number and the SQLSTATE |
| `database_too_large` | *garden* is too large for a branch. Branches work for databases up to *1 GB*. Make a branch with tables only. | Select "Tables only" |
| `statement_not_allowed` | Ruum doesn't run this kind of query (it changes roles, settings or files of the server). Leave that part out. | Note under the editor |
| `class_read_required` | This change reads or writes personal or secret columns, which only people who can manage this database may see. Leave those columns out. (Name *garden* and *column* once the UI has them.) | Note under the editor |
| several statements (`validation_failed` on a query) | Run one query at a time. This has *2*. | Note under the editor |
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
| Files (top), can edit | Your files go here | Upload files or make a folder to get started. Ruum keeps every change as a version, so nothing gets lost. | Upload files / New folder |
| Folder, can edit | This folder is empty | Plenty of room here. Drop files in, or upload them. | Upload files / New folder |
| Folder, view only | This folder is empty | | |
| Guest home | Shared with you in *Space* | You see only what people in *Space* shared with you. Nothing yet. | |
| Shared with me | Nothing shared with you yet | When someone shares a file or folder with you, it lands here. | |
| Trash | The trash is empty | Nothing to clean up. Items you move to the trash stay here for a while, so you can bring them back. | |
| Search, no results | Nothing matches "*q*" | Check the spelling, or try part of the name. | Search all of *Space* (when inside a folder) |
| Versions, one only | This is the only version | Ruum keeps a new version each time the file changes, so its history starts here. | |
| Tags on an item | No tags | | Add a tag (can edit) |
| People with access | Only you can see this | | (the add field is above) |
| Links | Anyone with the link: off | | Turn on |
| Agents | No agents yet | Bring in a helper: connect Claude, ChatGPT or another AI app, or create an agent with a key for a script. | Connect an agent |
| Agent activity | *Agent* hasn't changed anything yet | When it gets to work, its changes show up here, grouped by run. | |
| Agent keys | No keys yet | Apps that connect with sign-in (OAuth) don't need one. | New key |
| Members (only owner) | It's just you here | Invite the people you work with. | Invite people |
| Invitations | No open invitations. All quiet here. | | Invite people |
| Activity log | No activity yet | Changes in *Space* show up here. | |
| App passwords | No app passwords yet | Create one to open your files in Finder or Explorer. | New app password |
| Database tables (Query) | No tables yet | Tables come from migrations, or from a CREATE TABLE in Can change data. | |
| Query result | No rows match | | |
| Columns | All columns are confirmed | Agents see each column as you chose. | |
| Waiting for you | (section hidden) | | |
| Agent access, grants | No extra access | No agent has extra access to *garden*. Agents can read it, with personal data as stand-ins. | Give access |
| Branches | No branches yet | Make one to try changes without touching *garden*. | New branch |
| Migrations | No migrations yet | Migrations change the tables. They show up here with who ran them. | |
| Backups, no backup | No backup yet | Ruum makes the first backup within a few minutes. After that you can go back to any time in the last 7 days. | |
| Restore points | No restore points yet | Ruum makes one before each migration and each write access. | Name this moment (can edit) |
| Query log | No queries yet | Every query on *garden*, by people and agents, shows up here. | |
| Query log, filtered | No queries match these filters | | Clear filters |

## 6. Emails

Emails use the same voice, a little warmer than the UI, and calm about links and addresses
(people check emails for phishing). Subject lines say who and what. The server writes them in
`crates/ruum-core/src/tenants/mail.rs`; they are English until the server localises emails by the
recipient's language (design 12.4).

| Email | Subject | Body (plain text) |
|---|---|---|
| Invitation to a space | *Person* invited you to *Space* on Ruum | *Person* invited you to join the Ruum space "*Space*" as *a member*. / Open this link to join: *link* / The link works until *21 November 2026*. / Message from *Person*: *message* / If you didn't expect this email, you can ignore it. |
| Shared with you | *Person* shared "*Item*" with you | *Person* shared the *folder* "*Item*" with you on Ruum. You *can edit* it. / Message from *Person*: *message* / Open it: *link* / (email grant) To open it, sign in to Ruum with this email address (*address*). New to Ruum? Create an account with this address and verify it first. / If you didn't expect this email, you can ignore it. |

The HTML part says the same, with the link as "Join the space" or "Open the *folder*". Role words
in emails: "a guest (you see only what people share with you)", "a member", "an admin", "an
owner"; "can view", "can comment on", "can edit", "can manage".
