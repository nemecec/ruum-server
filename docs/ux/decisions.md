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
