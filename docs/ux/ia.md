# Information architecture

Status: `provisional`. Navigation, URLs and the names of things for the step 1 web UI. The names
also apply to emails, the CLI's human output and MCP tool descriptions where a person reads them
(`agent-experience.md` has the rules for agent-facing words).

## 1. Names: one word per concept

The design (`docs/DESIGN.md`) and the API use precise technical words. The UI uses one plain word
for each concept, and never two words for the same thing. The API word stays in the API, the
CLI's `--json` output and MCP field names.

| Concept (DESIGN.md / API) | UI word | Never in the UI | Notes |
|---|---|---|---|
| tenant | **space** ("personal space", "team space") | tenant, organisation, workspace, account | The sign-in page and the invitation email already say "space". A person has one personal space and can be in team spaces. |
| tenant kind `personal` / `team` | **personal space** / **team space** | | |
| node | **item** (when the kind does not matter); else **file**, **folder** | node, object, entry, resource | "3 items", "Move item". Use "file" or "folder" when it is known. |
| root folder of a space | **Files** | root, /, home | The top of the tree is called "Files" in the title and the breadcrumb, never the space name. |
| link `shortcut` | **shortcut** | alias, symlink | Step 1 UI shows shortcuts in lists; creating them is later. |
| link `member` | (not shown in step 1) | member link | Albums come later. |
| tag | **tag** | label, category | "label" is reserved: see version name. |
| version | **version** | revision, snapshot, commit | `revision` (the optimistic-lock counter) is never shown. |
| head version | **current version** | head, latest | |
| version `label` | **version name**; action "Name this version" | label, tag, pin | A named version is kept forever (design 5.3). Say so in the dialog. |
| `restoreVersion` | **Restore** (a version) | revert, roll back | Restore makes a new current version; nothing is lost. |
| diff | **Compare**; result "Changes" | diff, delta, patch | |
| diff plugin | **Compare as** (text, data, image, file) | plugin | Show the plugin name only in the details line of the result. |
| trash | **Trash**; action "Move to trash" | bin, recycle, delete (for trash) | |
| `restoreNode` | **Restore** (from the trash) | undelete, recover | The same verb as for versions: "bring it back". |
| purge, empty trash | **Delete for good**, **Empty trash** | purge, hard delete, delete forever | Only these two actions use the word "delete". |
| grant | **access**; list "People with access" | grant, ACL, permission entry | "Give access", "Remove access", "Change access". |
| role `view`/`comment`/`edit`/`manage` | **Can view**, **Can comment**, **Can edit**, **Can manage** | Viewer, Editor, Owner (for node roles) | Member roles (Owner, Admin, Member, Guest) are a different set: they are roles in a space, not on an item. |
| permission `share` | **can share** | share permission, sharer | Setting text: "People who can edit can also share". |
| `editors_can_share` = null | **Same as the folder above** | inherit, inherited from parent | |
| `inherited_from` | **from folder *Name*** | inherited, parent grant | Always name the folder and link to it. |
| share link | **link**; "Anyone with the link" | share link token, secret URL, anyone_with_link | "Copy link", "Turn off link". |
| `revokeShareLink` | **Turn off** (a link) | revoke, invalidate | People understand "turn off". |
| `tenant#member` grantee | **Everyone in *Space name*** | all members of the tenant | |
| guest | **guest** | external user | A guest sees only what is shared with them. |
| member role `owner`/`admin`/`member` | **Owner**, **Admin**, **Member** | | |
| invitation | **invitation**; action "Invite" | | |
| tenant policy | **Sharing rules** | policy, settings (alone) | The admin page "Sharing rules". |
| plan limits, usage | **Plan** | limits, quota | "Plan" page shows usage against the plan. |
| audit log | **Activity log** (space) | audit, journal | For admins. The per-agent list is **Activity**. |
| change journal | (not shown) | journal, feed, cursor | Users see it as "Activity". |
| agent | **agent** | bot, assistant, AI, integration, app | "Claude for Liis". Always with the agent mark (design-system.md). |
| `actor` + `on_behalf_of` | ***Agent* for *Person*** | "agent for a user", IDs | "Claude Desktop for Liis Tamm". |
| agent scope | **access** ("Agent access") | scope, permissions, subtree | Parts: "Where" (a folder or everything), "Highest role", "Ends on", "Changes per hour". |
| scope `subtree` | **Where**: "Only in folder *Name*" or "Everywhere you have access in *Space*" | subtree | |
| scope `max_role` | **Highest role** | max role | |
| scope `budget.max_writes_per_hour` | **Changes per hour, at most** | writes, budget | |
| API key `ruum_agent_...` | **key** ("agent key") | token, secret, API key (alone) | "Copy the key now. Ruum shows it only once." |
| access token | (never shown) | token | |
| OAuth client | **app** (the program that asks to connect) | client, OAuth client | Consent page: "Claude wants to connect to Ruum". |
| OAuth consent | **Connect** / **Allow** / **Don't allow** | authorize, consent, grant | |
| disable / enable agent | **Pause** / **Resume** | disable, enable | Paused agents keep their history and keys, which stop working until resumed. |
| `undoAgent` | **Undo changes** | revert, rollback | "Undo 12 changes by Claude". |
| app password `ruum_app_...` | **app password** (WebDAV) | | Account page, section "Connect Finder or Explorer". |
| session | **signed-in device** | session | Account page. |
| identity | **sign-in method** | identity, IdP | |
| content URL, content domain | (never shown) | | "Preview" and "Download" only. |
| upload | **upload** | transfer, put | |
| chunk, hash, manifest | (never shown) | | Upload panel shows "Checking", "Uploading", "Finishing". |

### Step 2: databases

Added for step 2 (`databases/README.md`). The same rules apply: one word per concept.

| Concept (DESIGN.md / API) | UI word | Never in the UI | Notes |
|---|---|---|---|
| typed node of type `database` | **database** | db, typed node, instance, resource | An item in Files with its own page. |
| `engine` `postgres` / `sqlite` | **Postgres** / **SQLite**; field label "Kind of database" | engine | One line of meaning under each choice (copy.md 3.8). |
| database manifest | **settings**; its versions **settings history** | manifest, config | The Versions tab of a database is called "Settings history", so nobody mistakes it for a backup of the data. |
| `restoreVersion` of a database | **Restore these settings** | | Never "restore" alone on that tab: "restore" of data is a restore point. |
| `DatabaseState` | **Setting up**, **Ready**, **Updating**, **Needs attention**, **Paused**, **Didn't start** | provisioning, degraded, suspended, failed | `deleted` is never shown (the item is gone). |
| statement, HTTP query API | **query**; the tab **Query** | statement, SQL console | Error copy may say "query" for any statement. |
| `mode: read` / `mode: write` | **Read only** / **Can change data** | read mode, write mode, RO, RW | |
| column | **column** | field, attribute | |
| data class | **privacy class**; values **Normal**, **Personal**, **Secret** | data class, sensitivity, label | Column header "Privacy". |
| unconfirmed column | **not confirmed** ("Personal until you confirm") | unclassified, pending | |
| class suggestion | **suggested** ("Suggested by *Agent* for *Person*: looks like an email address") | proposal, recommendation | |
| pseudonym | **stand-in**; chip "person 8f3a91" | pseudonym, hash, token, anonymised | Running text: "Agents see a stand-in, the same for the same value." |
| hidden-value marker | **hidden**; chip "hidden: secret" | masked, redacted, `****` | |
| `DataCopy` `masked` / `schema_only` / `full` | **masked copy** / **tables only** / **full copy** | anonymised copy, empty copy | "Masked copy: personal values become stand-ins, secrets are removed." |
| branch | **branch** ("a copy of the database to try changes") | fork, clone, sandbox, preview database | Explained once in the "New branch" dialog. |
| migration | **migration** ("a change to the tables") | schema change (alone), DDL | Shown to people in the approval and the Overview list. |
| restore point | **restore point** | snapshot, backup point, checkpoint | Automatic ones say why: "Before write access for Claude Code". |
| point-in-time restore | **Restore to an earlier time** | PITR, roll back, revert | |
| `target: new_node` / `in_place` | **Restore as a new database** / **Replace the data in *name*** | in place, new node | |
| `pitr_window` | "You can go back to any time since *5 Nov, 14:05*." | PITR window, retention | |
| `write_live` grant | **write access** ("Change data") | write grant, live write, write_live | Always with the end time: "Write access until 15:42". |
| `read_personal` grant | **personal data access** ("See personal data") | read_personal, PII grant | |
| `revokeDatabaseAccessGrant` | **End now** (a person); "gave it back" (the agent) | revoke | |
| `DatabaseApproval` | **request**; heading **Waiting for you**; actions **Approve** / **Deny** | approval request, ticket | Status words: Waiting, Approved, Denied, Ended, Used. |
| destructive statement | "a query that deletes or replaces data" | destructive statement, DDL | The request names the reason in words (copy.md 3.8). |
| query log | **Query log** | audit, statement log | Values in a query show as "value 1", "value 2" (they are never stored). |
| `my_permissions` | **Your access** (a sentence) | permissions | "You can read and change data. Only people who can manage it see secret values." |

Rules:

- **One word per concept.** If two screens need different words, the concept is different: add a
  row to this table first.
- **One concept per word.** "Restore" means "bring back" for versions and the trash. "Activity"
  means "a list of changes". "Access" means "what a person or agent may do here".
- **Verbs on buttons, nouns on headings.** "Give access" (button), "People with access"
  (heading).
- **The API word is allowed** in three places only: the details panel line "ID", the CLI with
  `--json`, and developer docs.

## 2. Navigation

### App shell (signed in)

```
+---------------------------------------------------------------------------------+
| [Ruum]  [Space: Liis Tamm v]        [ Search files and folders      ]  [LT v] |
+-----------+---------------------------------------------------------------------+
| Files     |  Files > Clients > Pärna 12                                         |
| Shared    |  ------------------------------------------------------------------ |
| Trash     |  page                                         | details panel       |
|           |                                               | (optional)          |
| Agents    |                                               |                     |
|           |                                               |                     |
| --------  |                                               |                     |
| Space     |                                               |                     |
| settings  |                                               |                     |
| (admins)  |                                               |                     |
+-----------+-----------------------------------------------+---------------------+
```

- **Primary navigation** (left rail on desktop, bottom bar on phone): Files, Shared, Trash,
  Agents. These four never change place.
- **Space switcher** (top left, next to the logo): the current space name and kind; a menu with
  every space ("Liis Tamm, personal", "Studio Linn, team, admin"), and at the end "Start a team
  space".
- **Space settings** (end of the rail, admins and owners only): Members, Invitations, Sharing
  rules, Plan, Activity log. Members see nothing here; they never see a link that ends in
  "You cannot open this page".
- **Account menu** (top right, the person's initials): Account, Sign out. Account holds profile,
  sign-in methods, signed-in devices and app passwords.
- **Search** (top centre): searches names in the current space. On the phone it is an icon that
  opens a full-width field.
- **Upload panel**: a fixed panel at the bottom right that stays open across pages while uploads
  run (design-system.md).

On the phone (under 768 px): the rail becomes a bottom bar with 4 items (Files, Shared, Trash,
Agents) and labels under the icons. Space settings and Account move into the account menu.

### Pages and URLs

URLs keep the current shape (`/t/<tenant id>/...`, as in `web/src/app/routes.tsx`). The `t`
segment stays, because it is in links already; the UI never says "tenant".

| Page | URL | Who | Title (h1) |
|---|---|---|---|
| Sign in | `/signin` | everybody | Sign in to Ruum |
| Welcome (first sign-in) | `/t/<space>/files?welcome=1` | new user | Welcome to Ruum (a card over Files) |
| Files (top) | `/t/<space>/files` | member, owner | Files |
| Folder | `/t/<space>/files/<folder id>` | anyone with access | the folder name |
| Item details | `...?item=<id>` (today `?node=`) | anyone with access | the item name (h2 in the panel) |
| Versions | `...?item=<id>&panel=versions` | view and up | Versions |
| Shared with me | `/t/<space>/shared` | everybody | Shared with me |
| Trash | `/t/<space>/trash` | edit and up | Trash |
| Search | `/t/<space>/search?q=` | everybody | Results for "q" |
| Agents | `/t/<space>/agents` | everybody | Agents |
| Connect an agent | `/t/<space>/agents/connect` | everybody | Connect an agent |
| Agent | `/t/<space>/agents/<agent id>` | its owner, admins | the agent name |
| Account | `/t/<space>/account` | everybody | Account |
| Space settings | `/t/<space>/settings/members` (today `/admin/...`) | admin, owner | Space settings |
| Open a link | `/s#<token>` | anybody, no sign-in | the item name |
| Accept an invitation | `/invitations/accept?token=` | anybody; sign-in needed to accept | Join *Space* |
| Connect an app (OAuth) | `/oauth/consent?request=` | signed-in user | Connect *App* to Ruum |
| Not found | any other URL | everybody | Page not found |
| Database (step 2) | `/t/<space>/databases/<id>` (tab Overview) | view and up | the database name |
| Database tabs | `.../databases/<id>/query`, `/columns`, `/access`, `/log`, `/backups` | see `databases/screens.md` | Query, Columns, Agent access, Query log, Backups |
| A request of an agent | `.../databases/<id>/access?request=<approval id>` | the agent's user, people who can manage | the database name; the request opens as a dialog |
| Give write access (prefilled) | `.../databases/<id>/access?give=write_live&agent=<agent id>&minutes=60` | the agent's user | the prompt opens as a dialog; nothing is given without a click |

The `/s#<token>` and `/invitations/accept` pages are the targets of the links that the server
already sends (share links and invitation emails). They are missing in the UI today
(gap-analysis.md, cards UX-01 and UX-02).

### What a guest sees

A guest in a team space (Helena, or Jonas in a client space) sees the same shell with:

- Files: "Shared with you in *Space*" (the items shared with them in that space), not the root.
- Shared, Trash (only items they can manage), Agents (their own).
- No Space settings, no "Start a team space" (that is in their own personal space).

A visitor with a link (no account) sees a reduced shell: the Ruum logo, the item name, the
breadcrumbs inside the shared folder, the list, and "Sign in" at the top right. No rail, no
search, no agents.

## 3. Page structure

Every page has the same order, so people learn it once:

1. Breadcrumbs (Files pages only).
2. Title (h1) and the page actions on the same line (primary action last, at the right).
3. An optional one-line explanation in muted text (only on Trash, Agents and settings pages).
4. Inline messages (errors that block the page).
5. The content (list, form, cards).
6. "Show more" at the end of a long list.

The details panel is the right column on desktop (min 1024 px), and a sheet that slides up over
the list on the phone and tablet. It has its own h2 and a close button. Its tabs are Details and
Versions; Share opens the share dialog.

## 4. Sorting, order and counts

- Folders first, then files; each group by name (natural sort: "Plan 2" before "Plan 10"). A
  person can sort by Name, Changed or Size; the choice is kept per person in the browser.
- Versions: newest first. Activity: newest first. Shared with me: newest share first.
- Counts in headings only when they help: "Trash (12)" no; "Undo 12 changes" yes.

## 5. Open questions (provisional answers)

| Question | Provisional answer | Why |
|---|---|---|
| "Space" or "workspace"? | Space | Shorter, already in the sign-in page and the invitation email, and "Ruum" means room or space. |
| Show IDs anywhere? | Only in the details panel, collapsed under "More facts", with a copy button | Jonas needs them for the CLI; Liis never opens it. |
| Grid view in step 1? | Yes, for folders where most items are images; list is the default | Liis has site photos. Rich previews for every type are not in step 1 (design 12.3). |
| Rename "Admin" in the URL? | `/settings/...` with a redirect from `/admin/...` | Old links keep working. |
