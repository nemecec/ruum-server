# UX decisions of the UX fix round (branch `q/uxfix`)

Status: `provisional`, 2026-11-06. This page records the decisions taken while the web UI was
brought in line with this package and the findings of the UX panel were fixed. It changes no
contract. The contract needs at the end go to the architect panel, with C-1 to C-6 of
`gap-analysis.md`.

## What the round did, by card

| Card | Done | Not done (and why) |
|---|---|---|
| UX-01 Share link | `/s#token` page: `resolveShareLink` with the token in the body, the password form, "doesn't work any more", the reduced shell, subfolders, Download and Preview with the link's access token (memory only, never in a URL) | Comments, upload through an edit link |
| UX-02 Accept invitation | `/invitations/accept?token=`: sign-in keeps the address, one click joins, "You've joined *Space*", the ended copy | The space's name before the click (contract need C-9) |
| UX-03 Connect an agent | `/t/<space>/agents/connect`: the MCP address with Copy, steps for Claude Desktop, Claude Code (`claude mcp add --transport http ruum <address>`) and ChatGPT, "Create an agent with a key"; consent defaults to "Only in a folder" and 90 days, with the access summary and "Connected" | Per-app screenshots |
| UX-04 Tokens | The colour tokens of design-system 2.1, light and dark, reduced motion | Type and spacing scales |
| UX-05 Naming | Space, Type ("Text file"), Items, Sharing rules, Plan, Activity log, Pause/Resume, Turn off, Keys, Access, version name | `/admin` stays the URL (UX-17) |
| UX-06 Error copy | Copy by code (`web/src/api/errors.ts`), the API message and hint only under "Details"; a unit test covers every `ErrorCode` of the contract | |
| UX-07 Empty states | The welcome card (once per person, `?welcome=1` shows it again), the empty states of copy.md 5 | |
| UX-08 Uploads | One panel at the bottom right for the whole session, drop zones on the list and on folder rows, one conflict question per batch, the saved name in the row, the leave-page warning | Pause and resume when offline |
| UX-09 Offline | The offline banner | The session-ended dialog |
| UX-10 Agent mark | The mark and "*Agent* for *Person*" in the list, details, versions, activity, the activity log | The "Changed by agents" filter (C-2) |
| UX-11 Destructive actions | Move to trash and Move without a confirmation, with Undo in the toast; Delete for good, Empty trash, Turn off link/key and Remove member keep one | Undo for Remove access |
| UX-12 File list | Checkboxes, Select all, a selection bar with Move and Move to trash, sort with `aria-sort`, natural order, folders first | Shift ranges, row click to select |
| UX-13 Share dialog | The owner first, names and emails instead of IDs, "from folder *Name*" with a link, "Invited, not joined yet", role meanings, the 7-day default visible, "Anyone with the link" | A people picker; the link switch; the "Shared" mark (C-1) |
| UX-14 Agent page | The access summary with "Change access", activity rows with the item's name and a link, undone rows marked, Pause with its confirmation | Grouping by runs |
| UX-15 Checks | `web/e2e/visual.e2e.ts` (22 screens, 4 variants, snapshots, axe) and `web/e2e/a11y.e2e.ts` (axe with the real server) | Snapshots for Linux CI |
| UX-16 Versions | Compare in a wide dialog (a full page on the phone) in plain words, "Restored from *date*", Restore with Undo, "Name this version" with the kept-forever line, the one-version empty state | Day groups |

## Decisions

1. **"Send an email" is off for shares.** The server sends no email for `addGrant`
   (`crates/ruum-api/src/routes/sharing.rs`), so the UI sends `notify: false` and shows the
   check box disabled, with "Ruum can't send emails for shares yet". Turn it on when the server
   sends the J3 email.
2. **Names come from what the person may list.** Until the server fills `display_name` (C-7),
   `web/src/app/names.tsx` resolves user IDs through `listMembers` (members and up) and agent IDs
   through `listAgents`, and loads them again for an unknown ID. Unknown people show as
   "Someone" (or "Someone in *Space*" on Shared with me), unknown agents as "An agent". The UI
   never shows an ID. The share dialog also remembers, per browser, the email it typed for an
   email grant, so the row keeps a name after the grant binds.
3. **A space that is not in the list opens with only what was shared.** A shared item links to its own space
   (`/t/<space>/files/<id>` for a folder, `/t/<space>/item/<id>` for a file). When the space is
   not in the person's list (an email grant that bound without a guest membership, a server
   gap of `docs/TESTING.md`), the page says "Ruum can't open this for you yet" instead of an
   error. That space shows as "*Space* (shared with you)" in the switcher, without the
   "You're a guest here" banner: the banner is only for a real guest of a listed space.
   A share by email with a person outside the space also sends a guest invitation (an option of
   the share dialog, on by default, for admins of a space that allows guests); without it the
   dialog says the person must join first, and a grant row of a user who is not in the space
   says they can't open it yet.
4. **Coalescing stays on the server; the UI warns.** "Upload new version" by the same person
   within 10 minutes of their last unnamed save asks first: name the current version (named
   versions never join) or replace it. The name-conflict dialog of an upload does the same: for
   such a file it says so, calls the choice "Replace your version from ...", makes "Keep both"
   the default, and offers to name the current version. After an upload that joined, the toast
   says so. A real fix is a flag on the upload (C-8).
5. **The consent screen reads the sealed request.** The server seals the authorize parameters
   as `{iss, aud, iat, exp, v: {client_id, redirect_uri, ...}}`; the UI reads `v` (and the top
   level as a fallback) for the client and its redirect host, so the screen and the new agent
   carry the app's name.
6. **A personal space invites guests only.** The plan of a personal space has one member, so
   Invitations offers only Guest there, says why, and offers "Start a team space". Team spaces
   start from the space switcher (an option at the end of the list) and from Account.
7. **Links stay shown once.** C-6 keeps its provisional answer. The dialog says so before the
   person creates a link.
8. **Toasts, not page notices.** Results show as toasts at the bottom that go away after 6 s
   (10 s with Undo); they stop while the pointer or the focus is on them. Notes inside a dialog
   stay inline.
9. **The upload panel folds** to its header 4 s after everything is done, so it does not cover
   the page; it stays open while an upload runs or needs the person.
10. **Visual snapshots run on the mock API** with a fixed clock (2026-11-06 10:00 UTC, en-GB,
   Europe/Tallinn), so they do not change between runs. They are made on macOS; the file names
   carry the platform. A Linux CI job needs its own set (`--update-snapshots` once, reviewed).

## Contract needs found in this round

| ID | Need | Why | Proposal |
|---|---|---|---|
| C-7 | Names on actors and grantees | Lists, versions, the activity log and the share dialog must show "Claude Code for Ann", never an ID (J3, J4, J5 success criteria). The contract says `display_name` is "filled when known"; the server never fills it | Fill `PrincipalRef.display_name` (users and agents) and `GranteeUser.display_name`/`email` in every answer |
| C-8 | A new version that never joins the version before | "Upload new version" is an explicit save; joining it into the head loses the old content | `coalesce: false` on `UploadTargetNewVersion` (and `putContent`), the web UI sends it |
| C-9 | What an invitation is for, before it is accepted | The accept page should say "*Person* invited you to *Space*" (copy.md 3.6) | `GET /v1/invitations/preview?token=` (public, rate limited): space name, inviter name, role |
| C-10 | The space of a shared item | Shared with me must name the owner's space also when the person is not a member there | `tenant_name` on `SharedItem` (and the sharer's `display_name`, see C-7) |
| C-11 | Who changed an item later | The undo dry run says "it was changed again later"; journeys J5 want the person's name | `changed_by` (`ActorPair`) on `UndoItem` with reason `changed_later` |

Server gaps seen again (not UI work): an email grant that binds at sign-in does not make the
person a guest, so every read answers `404`; an email grant to an existing account never binds
(`docs/TESTING.md`, known gaps).

# Synthetic-user round (branch `q/synthfix`)

Status: `provisional`, 2026-11-06. The synthetic users (personas Kati, a garden designer in a
personal space, and Jaan, the admin of the team space "Tamm Consulting") found 2 blockers,
6 majors and a list of minor points. This round fixes the blockers and the majors in `web/` and
some minor points. It changes no contract and no server code. Decisions 1, 3 and 4 above are
replaced by decisions S1 to S3 below.

## Decisions

S1. **A share to a person of the space goes to their account.** The share dialog looks up the
   typed email in the space's people (`listMembers`; members and up may list them). For a member
   or guest it sends `addGrant` with `{type: "user", id}` (decision 0002-1), so the item shows
   at once under their Shared with me. Before, it sent an email grant, which by the contract stays
   an email grant until an invitation is accepted, so it never reached a person who had joined
   already (the blocker). A guest who shares cannot list the people; the dialog then still sends
   an email grant (contract need C-12). The mock API now keeps an email grant as an email grant,
   as the server does; it hid the blocker before.

S2. **The dialog shows access that comes from the space role, and says when a share can't limit
   it.** In a team space, owners, admins and members manage everything through the team root
   (decision 0001-4), and share there while `members_can_share` is on. "People with access" now
   starts with "Everyone in *Space* except guests: Can manage" and how many people that is. The
   add form warns when the email is a member's ("A share can't give them less. To let them see
   only some folders, make them a guest"), a grant row of such a person says the role wins, and
   the sharing rule says it does not apply to them. The role help says what Member means
   ("can see, change, delete and share everything in the team space") and that Guest is the role
   for per-folder limits. A change to Guest on the Members page asks first and says what the
   person loses. A member role that can only view is a product question for the architect panel
   (not a UI change).

S3. **The share dialog says only what Ruum does.** The disabled "Send an email" box and "Ruum
   can't send emails for shares yet" are gone. For a person outside the space the one box is
   "Email them an invitation" (on by default, admins of a space that allows guests): "*email* is
   new here. With the invitation they make a free Ruum account, then they can open *item*." The
   words "space" and "guest" are not used for that person. "Use a link instead" moves the focus
   to the link form, the way that needs no account. For a person of the space the form says Ruum
   sends no email and they find it under Shared with me. The invitation's message leads with the
   item: `I shared "<item>" with you. Accept this invitation and make a free Ruum account to open
   it.` The email itself still starts with the space (server template, see C-13).

S4. **A conflict upload keeps every version by default.** "Save as a new version" is the default
   choice. When the current version is the person's own unnamed save of the last 10 minutes,
   the server would join the upload into it (design 5.3), so the dialog names that version first
   ("Before the upload of 15:13", editable) and says "Ruum keeps that version under this name, so
   nothing is lost". Named versions never join and are kept forever; that is the cost of this
   default, and it is visible in Versions. If the person clears the name, the dialog says in
   plain words that the upload replaces the version. "Keep both" says that it makes two files.
   C-8 (`coalesce: false`) stays the real fix; with it the automatic name can go.

S5. **The activity log names what changed.** Access, role, sharing rule, link and member entries
   are written from the entry's `data` (the server already stores the grantee, the role, the old
   and new role or rule) and `target_id`: "Ada Lovelace gave Grace Hopper edit access to
   *engine-specs.txt*", "changed Grace Hopper from Guest to Member", "changed the sharing rule of
   *X*: people who can edit can also share, from same as the folder above to no". The item is a
   link (name from `getNode`; an item the admin can't read stays "an item"). People come from
   the names directory (C-7 would make this complete for removed people).

S6. **Smaller fixes of this round.** The sharing rule explains itself, its inherit choice shows
   the parent's effective value ("Standard" at the top level), and "Saved." confirms the save. A
   person's own grant row has no controls; a grant added by someone other than the owner says
   "added by *name*". Versions show the time of day with the relative age ("13 minutes ago
   (15:13)", "20 Oct 2026, 14:00"); "Name" is "Keep forever..."; Compare says "3 changes" and
   keeps "Compare as" behind "More options". Images have a picture icon. The sign-in button that
   opens the Zitadel login is "Sign in with email", and a sign-in on the way to an invitation says
   so. The welcome card no longer promises folder upload. The accept page says that the
   invitation email names the space, the person and the role (C-9 stays open).

## Findings of the round

| Finding | Severity | State |
|---|---|---|
| Share by email to a member or guest stays "Invited, not joined yet" | blocker | Fixed (S1); e2e `sharing.e2e.ts` shares again with Ben after he joined |
| Member role gives "Can manage" everywhere; share and rule don't limit it, no warning | blocker | Fixed in the UI (S2): shown, warned, role help, confirm before Guest. A view-only member role needs the architect panel |
| Dialog says Ruum sent an invitation and that it can't send emails | major | Fixed (S3) |
| Sharing with a client becomes "join the space as a guest", account needed | major | Fixed in the dialog (S3); the email still leads with the space (C-13); a share email without an account needs C-13 too |
| Conflict dialog reads as "your earlier note is lost"; "Keep both" default makes copies | major | Fixed (S4) |
| "People with access" hides role access | major | Fixed (S2) |
| Activity log says "gave access: an item" | major | Fixed (S5) |
| Accept page names nothing | major | Not fixed: needs C-9. The copy now points to the email |
| Sharing rule inherit option shows the folder's own value, no save confirmation | minor | Fixed (S6) |
| Rule question on a top-level folder of a personal space | minor | Explained in place (S6), not hidden: in a personal space it still governs guests with Can edit |
| Versions show only a relative age; "Name" unclear; Compare in technical words | minor | Fixed (S6) |
| Photos and notes have the same icon | minor | Fixed (S6) |
| "Upload files and folders" but no folder upload | minor | Text fixed (S6); folder upload is not built |
| "Sign in with a passkey" opens a login name and password | minor | Label fixed (S6). The Zitadel login theme ("Login Name") and the 2FA prompt are Zitadel settings of `scripts/dev.sh`, not changed here |
| Sign-in from an invitation link gives no context | minor | Fixed (S6) |
| Member role change without confirmation | minor | Fixed for a change to Guest (S2); no Undo |
| Share rows: who added a grant; own row editable | minor | Fixed (S6) |
| No sign-ins in the team space's activity log | minor | Not fixed: no server code calls `audit::events::sign_in`, so no space records a sign-in (C-14) |
| Invitation email is plain text, no role or end date | minor | Not fixed: server template (C-13) |
| `scripts/ux/drive.mjs` `fill` output | minor (tooling) | Not changed |

## Contract and server needs found in this round

| ID | Need | Why | Proposal |
|---|---|---|---|
| C-12 | An email grant to a person of the tenant binds at once | Agents (MCP, CLI) and guests who share, who cannot list members, still make email grants that never reach a member or guest (the blocker of S1 outside the web UI) | `addGrant` with the email of a member or guest of the tenant stores a grant to that user (a contract change: the text says it "stays an email grant") |
| C-13 | Share and invitation emails that lead with the item | Kati's client should read "Kati Tamm shared Garden Parna 12 with you", and should get a view link without an account if Kati wants that; the role and the end date belong in the invitation email | Implement `notify` of `addGrant` (the J3 email: item name, sharer, an "Open" button, HTML part); an invitation created from a share carries the item; the invitation email names the role and the end date |
| C-14 | Sign-ins in each space's activity log (server gap, no contract change) | An admin asks "who signed in to Tamm Consulting"; the filter "Sign-ins" is always empty | Call `audit::events::sign_in` at `loginCallback` for every tenant of the person (or record the first use of a session per tenant) |
| C-9 | (again, now a major) | The accept page can't name the space, the inviter or the role | As proposed above |

# Round 3: the UX panel of the design system (`w/design-system`, 2026-11-07)

Status: `provisional` (design 18.9). The panel files are `ruum-server-wt/panel/ux-heuristics.md`,
`ux-accessibility.md` and `ux-consistency.md`. All blockers and majors are fixed on
`w/design-system`, with the UI parts of decision 0003 items 1 to 4 and 7 (`panel/ui-followups.md`).

## Decisions

| ID | Decision | Why |
|---|---|---|
| D1 | Estonian keeps "ruum" for a space (i18n.md). The product name is not written in running Estonian text, and labels that could be read as "of the space" are renamed: the language picker is "Kasutajaliidese keel", the brand link is named "Ruum, failid", the space switcher is named by the space and "(vali ruum)", storage is "salvestusruum", and "Sind kutsutakse ühte ruumi". A person who speaks Estonian should confirm it; the catalogue stays `machine`. | H2, A4: "Ruum" (brand) and "ruum" (space) clashed in the top bar, on Account, in Trash and on the invitation page. A new word for space would change every screen and the decided term. |
| D2 | Code never changes the case of text. A message that sits inside a sentence has its own key (`format.when.*.inline`, `role.*.inline`, `memberRole.*.inline`, `diff.plugin.*.inline`); `npm run lint` refuses `toLowerCase`/`toUpperCase` in screens and `src/ui` unless the line says `// i18n-ignore` (data, not text). | H3, M2: "last used thursday at 06:20 pm"; languages that capitalise nouns. |
| D3 | Actions: two or three main buttons, the rest in the "More" menu (details panel, version rows); a role is chosen from a role menu with one meaning per role (members, content roles, share rows); row actions on the phone are a row menu ("More actions for *name*"); the space switcher is a menu button. | H4, H8, M6, consistency 1 and 3. |
| D4 | Loading: nothing for 300 ms, then a skeleton or "Loading..."; an error that a new try can help offers "Try again". | H5 and the common states of journeys.md. |
| D5 | Toasts are added into two live regions that exist from the start (polite, and assertive for errors). Errors stay until closed; a toast with an action stays 30 s; Alt+T focuses the newest toast. | A3. |
| D6 | The language applies with "Save" on the Account page; the focus returns to the picker and a status says the new language. | A1 (WCAG 3.2.2, 2.4.3). |
| D7 | The details panel on the phone is a modal sheet: the list is `inert`, Escape closes it, the focus goes back to the row. | A2 (WCAG 2.4.11). |
| D8 | The catalogue's sample text goes through the pseudo-locale with `&locale=en-XA` (`sample()` in `src/ui/story.ts`); the catalogue and design-system.md use the screens' wording ("Claude Desktop (agent of Liis Tamm)", "2 hours ago"); the file list keeps its "Details" button. | M1, H9. |
| D9 | Agent activity is shown in runs (changes less than 15 minutes apart) with "Undo these changes" on each run; the confirm lists the changes with the words and in the order of the list. | H7 (J5). |
| D10 | Members (not guests) see the Members page read-only, with the content roles; the other admin pages stay admin-only. | ui-followups item 1. |
| D11 | The invitation link carries its token in the fragment (`#token=`); the accept page keeps it in session storage over the sign-in and also reads `?token=` of old mails. | ui-followups items 2 and 3. |

## Not done in this round

- Decision 0003 item 6 (C-8 to C-11) was built later, on branch `f/ux-c8-c11`: uploads of a new
  version send `coalesce: false`, so the "Keep your last change" dialog, the name field of the
  conflict dialog and the automatic name "Before the upload of HH:MM" are gone (C-8); the
  sign-in and accept pages name the space, the inviter, the role, the end date and the masked
  address, and the ended states (C-9); Shared with me and the shell name a space from
  `SharedItem.tenant_name` (C-10); the undo confirm says who changed an item later (C-11).
- Optional cards of items 4 and 7: the search "Type" filter and bulk move and tag with
  `moveNodes` / `attachTagToNodes` need `f/audit-mcp`, which this branch does not include.
- Minors left as cards: versions grouped by day, the compare summary in lines, the consent form's
  nested fieldsets and its native date field, the admin tab repeated as a heading, helper text
  sizes, the toolbar order in the pseudo-locale, the contrast tool pairs (A10).
