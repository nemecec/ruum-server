# Design system

Status: `provisional`. The tokens and components of the Ruum web UI for step 1, and the database
components of step 2 (section 4b). The target is WCAG 2.2 level AA everywhere. The names of
things follow `ia.md`; the strings follow `copy.md`.

## 1. Identity

Ruum keeps people's work and lets agents touch it. The look must say **calm, trustworthy,
exact**. The brand is **Graphite** (decision 0005 B-5), built as the first theme package (design
12.6, `web/src/design/themes/graphite/`).

- **Calm.** Few colours, much space, one primary action per view, no motion that is not feedback.
- **Trustworthy.** Plain words, the truth about state (saved, uploading, offline), every
  destructive action explains how to get things back, agents are always visible.
- **Exact.** Cool graphite neutrals; a deep fjord blue as the primary colour; a raspberry plum
  for agents that appears nowhere else; square corners and thin rules; IBM Plex Sans for text and
  IBM Plex Mono for sizes, times and keys, so numbers read like facts. The playfulness lives only
  in the copy: sentence case and a human voice.

**Not the generic AI look.** No purple-to-blue gradients, no sparkle or magic-wand icons for
agents, no glass blur, no glowing borders, no dark-mode-first neon, no emoji in the UI, no
"AI-powered" badges. An agent is a worker with a name, not magic: its mark is a small solid
key glyph in plum, the same everywhere.

**Not a copy of another product.** The tokens below replaced the GitHub Primer palette of the
first UI. Section 6 says where they live in code.

## 2. Tokens

The tokens are defined once, in the W3C Design Tokens format, in `web/src/design/tokens/`
(section 6), and the default theme package (`web/src/design/themes/graphite/`) gives many of them
the Graphite values. The tables below are the effective values. Style Dictionary makes them CSS custom properties on `:root`. Components use only
tokens, never raw values. Dark mode follows `prefers-color-scheme`; a person can force light or
dark on the Account page (`data-theme` on `<html>`). Every text pair below is checked by
`docs/ux/tools/contrast.mjs` and by the theme validator `web/scripts/check-theme.mjs`, which read
the token files with the theme over them (WCAG 2.2: 4.5:1 for text, 3:1 for control borders,
icons, tag colours and the focus ring). All 84 pairs pass; the lowest text pairs are text-faint on
canvas in light (5.02:1) and text-faint on surface in dark (5.50:1).

### 2.1 Colour

| Token | Light | Dark | Use |
|---|---|---|---|
| `--color-canvas` | `#F6F7F8` | `#0F1215` | Page background (cool graphite) |
| `--color-surface` | `#FFFFFF` | `#171B20` | Cards, panels, dialogs, the list |
| `--color-sunken` | `#ECEEF1` | `#0B0D10` | Top bar, rail, code, table header, compare background |
| `--color-border` | `#D9DDE2` | `#2A3038` | Dividers and card edges (decorative, no contrast need) |
| `--color-border-control` | `#7B828C` | `#6F7883` | Borders of inputs, selects, checkboxes (3:1) |
| `--color-text` | `#16191D` | `#E6E9ED` | Body text, headings |
| `--color-text-muted` | `#4B525B` | `#A3ABB5` | Secondary text, metadata |
| `--color-text-faint` | `#646B75` | `#8A929C` | Placeholders, timestamps (still 4.5:1 on surface) |
| `--color-primary` | `#0B5C86` | `#6CB8E6` | Primary buttons, selected nav, focus ring (fjord blue) |
| `--color-primary-hover` | `#084A6C` | `#8FCBEF` | Hover and pressed primary |
| `--color-on-primary` | `#FFFFFF` | `#06202E` | Text on primary |
| `--color-primary-soft` | `#E1EEF6` | `#13293A` | Info notes, selected rows, drop zone |
| `--color-link` | `#0B5C86` | `#7CC2EC` | Links (always underlined in running text) |
| `--color-focus` | `#0B5C86` | `#6CB8E6` | Focus ring |
| `--color-selected` | `#E1EEF6` | `#152A3A` | Selected row background |
| `--color-danger` | `#B3261E` | `#FF8F85` | Errors, destructive buttons |
| `--color-danger-hover` | `#921F18` | `#FFA9A1` | Hover of danger buttons |
| `--color-on-danger` | `#FFFFFF` | `#2A0B08` | Text on danger buttons |
| `--color-danger-soft` | `#FBE6E4` | `#3A1A18` | Error note background |
| `--color-success` | `#1D6B45` | `#6FCF97` | Success text and icons |
| `--color-success-soft` | `#E2F1E8` | `#142B1E` | Success note background |
| `--color-warning` | `#7D5400` | `#E8B860` | Warnings (unverified app, plan nearly full) |
| `--color-warning-soft` | `#FBF0D6` | `#30260F` | Warning note background |
| `--color-agent` | `#8F3F71` | `#E79AC7` | The agent mark and agent names (raspberry plum) |
| `--color-agent-soft` | `#F6E6F0` | `#36202E` | Agent badge background, agent rows in Activity |

Rules:

- **Colour never carries meaning alone.** Errors have an icon and words; diff insertions have a
  `+` and the word "Added" for screen readers; the agent mark is a glyph and a name.
- **Tag colours** come from a fixed set of 8 (spruce, moss, clay, brick, ochre, slate, plum, sand)
  with a light and a dark value each (`--color-tag-*`), used as a 3 px left edge and a dot, never
  as text colour. The API's free `color` value maps to the nearest of the 8 (`nearestTagColor`).

  | Tag | Light | Dark |
  |---|---|---|
  | spruce | `#1D5C57` | `#72C2B9` |
  | moss | `#2B6439` | `#82C291` |
  | clay | `#8A4718` | `#E9A672` |
  | brick | `#A3301C` | `#F08B74` |
  | ochre | `#7F5300` | `#E2B455` |
  | slate | `#4A5868` | `#9AA8B8` |
  | plum | `#6E3B5E` | `#C99BBC` |
  | sand | `#8A7A55` | `#C8B78E` |
- **Diff colours**: insertions on `--color-success-soft` with `--color-success` text, deletions
  on `--color-danger-soft` with `--color-danger` text and a strike-through.

### 2.2 Type

In code the type scale is `--font-size-*` and `--font-line-*`, and `--text-*` is the `font`
shorthand of each step (weight, size, line height, family).

IBM Plex Sans for text and titles, IBM Plex Mono for code, keys, sizes and times (SIL Open Font
License). The fonts are self-hosted in the theme package (WOFF2, latin and latin-ext: Estonian,
German, Finnish, Polish), so nothing is sent to a font host, which matters for EU customers. The
system stacks stay as the fallback.

```css
--font-family-sans: "IBM Plex Sans", system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", sans-serif;
--font-family-display: var(--font-family-sans);   /* titles (h1, h2) */
--font-family-mono: "IBM Plex Mono", ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
--font-family-numeric: var(--font-family-mono);   /* sizes, times and counts in lists */
```

| Token | Size / line height | Weight | Use |
|---|---|---|---|
| `--text-xs` | 12 / 16 px | 400 | Badges only (never body text) |
| `--text-sm` | 14 / 20 px | 400 | Metadata, table cells on phone, helper text |
| `--text-md` | 16 / 24 px | 400 | Body, inputs, buttons, table cells (the base) |
| `--text-lg` | 18 / 26 px | 600 | Panel and dialog titles (h2) |
| `--text-xl` | 22 / 30 px | 600 | Page titles (h1) on phone |
| `--text-2xl` | 26 / 34 px | 600 | Page titles (h1) on desktop |

Rules: sentence case everywhere (no Title Case, no ALL CAPS except the file extension in a type
badge). Numbers in tables use `--font-family-numeric` and `font-variant-numeric: tabular-nums`. Line length of running text at
most 72 characters. Text can grow to 200% (browser zoom) without loss of content; the layout
reflows at 320 CSS px (WCAG 1.4.10).

### 2.3 Spacing

A 4 px base. Use only these steps.

| Token | Value | Typical use |
|---|---|---|
| `--space-1` | 4 px | Icon to text |
| `--space-2` | 8 px | Inside compact controls, between related items |
| `--space-3` | 12 px | Cell padding, between form label and field |
| `--space-4` | 16 px | Card padding on phone, gutter on phone, gap between fields |
| `--space-5` | 24 px | Card padding on desktop, between sections |
| `--space-6` | 32 px | Between page blocks |
| `--space-7` | 48 px | Empty states, top of the sign-in page |

Layout: rail 224 px, content max 1200 px, details panel 360 px (min 1024 px window), page gutter
16 px (phone) and 24 px (desktop).

### 2.4 Radius, elevation, borders

| Token | Value | Use |
|---|---|---|
| `--radius-sm` | 2 px | Badges, tags, checkboxes |
| `--radius-md` | 3 px | Buttons, inputs, rows on hover |
| `--radius-lg` | 4 px | Cards, panels, dialogs, toasts |
| `--radius-full` | 999 px | Avatars and the space switcher pill |
| `--shadow-1` | `0 1px 2px rgb(22 25 29 / 0.08)` | Cards (light only; dark uses a border) |
| `--shadow-2` | `0 8px 24px rgb(22 25 29 / 0.14)` | Dialogs, menus, toasts, the upload panel |
| `--border-1` | `1px solid var(--color-border)` | Dividers |

### 2.5 Motion

| Token | Value | Use |
|---|---|---|
| `--motion-fast` | 120 ms `cubic-bezier(0.2, 0, 0, 1)` | Hover, press, focus |
| `--motion-base` | 200 ms `cubic-bezier(0.2, 0, 0, 1)` | Panels, dialogs, toasts in |
| `--motion-exit` | 150 ms `cubic-bezier(0.4, 0, 1, 1)` | Things that leave |

Sizes, layers and other values that components need are tokens too: `--size-*` (control 36 px,
touch 44 px, rail 224 px, content 1200 px, panel 360 px, dialog widths, icons), `--border-width-*`
(1, 2 and 3 px), `--z-*` (sticky, sheet, upload, toast, banner, skip), `--opacity-disabled` and
`--breakpoint-*` (640 and 900 px; CSS repeats these in media queries, because a custom property
cannot be read there).

Rules: motion only as feedback (something opened, moved, finished). No looping animation except
progress. With `prefers-reduced-motion: reduce`, all transitions become opacity fades of 0 ms to
120 ms, and progress bars stay but do not shimmer. A new row highlights with
`--color-primary-soft` that fades over 2 s (no movement).

### 2.6 Icons

One outline icon set at 20 px, drawn in `web/src/ui/icon/`. The stroke comes from the tokens
`--icon-stroke-width` (1.5), `--icon-linecap` (square) and `--icon-linejoin` (miter), the same for
every line of every icon. Icons sit beside a text label;
icon-only buttons have an `aria-label` and a tooltip with the same text, and a 44 x 44 px target.

The **agent mark** is the one custom glyph: a solid rounded square with a small key-shaped cut
(an agent works with a key you gave it), 16 px, in `--color-agent`. It is never animated, and it
is never a robot, a sparkle or a magic wand.

## 3. Global accessibility rules (WCAG 2.2 AA)

These hold for every component below.

1. **Keyboard.** Everything works with the keyboard; the order follows the visual order; no
   keyboard trap except inside an open dialog (which closes with Escape).
2. **Focus visible** (2.4.7, 2.4.11): a 2 px `--color-focus` outline with 2 px offset on every
   focusable element; the focused element is never hidden under the top bar, the upload panel or
   a toast (2.4.11 Focus Not Obscured).
3. **Target size** (2.5.8): at least 24 x 24 px; our standard is 44 x 44 px on touch and 36 px
   high on desktop.
4. **Names.** Every control has a visible label or, for icon buttons, an `aria-label` that starts
   with the visible tooltip text (2.5.3 Label in Name).
5. **Status messages** (4.1.3): toasts and "Saved" messages use `role="status"`; errors that
   block use `role="alert"`; never both on one message.
6. **No drag-only actions** (2.5.7): drag to upload and drag to move always have a button
   alternative ("Upload", "Move").
7. **Consistent help** (3.2.6): the help link "How Ruum works" is in the account menu on every
   page, at the same place.
8. **Redundant entry** (3.3.7): a form never asks again for what it knows (the email of the
   person, the current folder).
9. **Accessible authentication** (3.3.8): sign-in needs no puzzle and no memory test; passkeys
   and providers are fine; the link password field allows paste and password managers.
10. **Reflow** at 320 px, text spacing overrides, and 200% zoom without loss.
11. **Language.** `<html lang>` matches the UI language; file names keep their own direction
    (`dir="auto"`).
12. **Tests in CI** (design 18.4.1): axe on every main screen in both themes, and keyboard paths
    for J1-J6 (and J7-J14 of `databases/journeys.md`) in Playwright.

## 4. Components

Each component lists its anatomy, states and accessibility rules. "States" always includes:
default, hover, focus, active, disabled, loading, error, and where it matters empty, no
permission, offline.

### 4.1 App shell

**Anatomy.** Top bar (`--color-sunken`, 56 px): logo "Ruum" (wordmark, links to Files), space
switcher, search, account menu. Left rail (224 px, desktop) or bottom bar (phone): Files, Shared,
Trash, Agents; Space settings at the end for admins. Main region. Optional details panel. Upload
panel and toast region fixed at the bottom right. Offline banner under the top bar.

**States.** Current page: the nav item has `--color-primary-soft` background, `--color-primary`
text, a 3 px left bar, and `aria-current="page"`. Offline: the banner (`--color-warning-soft`)
pushes the content down; it does not cover it. Session ended: a dialog (see 4.12).

**Accessibility.** Landmarks: `header`, `nav aria-label="Main"`, `main`, `aside
aria-label="Details"`. A "Skip to content" link is the first focusable element. The space
switcher is a menu button (`aria-haspopup="menu"`), not a native `<select>`, so it can show the
space kind and role; its button text is "Space: *Name*". The rail does not move focus on route
change; the new page moves focus to its h1 (`tabindex="-1"`) and announces the title.

### 4.2 Space switcher

**Anatomy.** A pill button with the space initial in a square, the space name, and a chevron.
Menu: one item per space with name and a muted second line ("Personal space", "Team space, you're
an admin", "Guest"), a check on the current one; a divider; "Start a team space".

**States.** One space only: the pill is not a button; the name is plain text, and "Start a team
space" is in the account menu. Long names truncate with an ellipsis and a tooltip.

**Accessibility.** Menu pattern (Up and Down arrows, Home, End, Enter, Escape, type-ahead).

### 4.3 Breadcrumbs

**Anatomy.** `Files > Clients > Pärna 12`. The first crumb is always "Files" (never the space
name). The last crumb is the current folder, not a link, with `aria-current="page"`. Deep paths
collapse the middle into "..." (a menu with the hidden crumbs) when the row is full.

**States.** Top level: one crumb "Files", hidden visually because the h1 says "Files". Shared
folder without access to its parents: the first crumb is "Shared with me", then the shared
folder. Link visitor: the first crumb is the shared folder.

**Accessibility.** `nav aria-label="Breadcrumbs"` with an ordered list; the separators are CSS,
not text.

### 4.4 File list and grid

**Anatomy (list).** A table: checkbox, kind icon (folder, file type, shortcut), name (with a
"Shared" mark icon and the tag dots), Changed (relative time and who: "2 hours ago, Claude Desktop (agent of Liis Tamm)"
with the agent mark), Size (files only), a "Details" button ("Details of *name*") that opens the
details panel, where the actions are (the main ones as buttons, the rest in its "More" menu). On
the phone the Changed column moves under the name as a second line, with the agent mark. Folders
first. Column headers are sort buttons. Above the list: the selection bar when something is
selected ("3 selected", Move, Download, Move to trash, Clear), and filter chips ("Changed by
agents").

**Anatomy (grid).** Cards of 160 px: a thumbnail for images (from the content URL, lazy), the type
icon for others, the name (two lines at most), and the checkbox at the top left on hover or
focus. A toggle "List" / "Grid" above the list; the choice is kept per folder in the browser.

**Behaviour.** Click on the name: folder opens, file opens its details. Click on the row (not the
name): selects the row and opens the details. Double click on a file: preview. Enter on a
focused row: open. Space: toggle selection. Shift-click and Shift-arrows: range select. The
separate "Details" button per row goes away (it is the row itself now).

**States.** Hover: row background `--color-sunken`. Selected: `--color-selected` plus the checked
box. Focused: the focus ring around the row. Loading: 6 skeleton rows. Empty: see 4.11. New
item: 2 s highlight. Trashed by someone else while open: the row fades out and a toast says
"*Name* was moved to the trash by *person*." View only: no checkboxes for actions that need
edit (Download still works), no drop zone. Offline: the list stays; the row menu shows actions
disabled with "You're offline".

**Accessibility.** List view is a real `<table>` with a caption ("Items in *folder*") and `scope`
on headers; sort buttons say the state (`aria-sort`). Grid view is a list (`ul`) of cards with
the same row menu. The name is the accessible name of the row; the Changed cell reads "Changed 2
hours ago by Claude Desktop, an agent for Liis Tamm". Checkboxes are labelled "Select *name*".

### 4.5 Upload panel

**Anatomy.** A panel fixed at the bottom right (360 px; full width at the bottom on the phone),
with `--shadow-2`. Header: "Uploading 4 of 12" (or "12 uploads done"), an overall progress bar,
a collapse button and a close button (close only when nothing runs). Rows: file name, target
folder (link), size, status text and a progress bar; per row: "Cancel" while running, "Try
again" after an error, "Show" after done (opens the folder with the item selected).

**States.** Waiting, Checking (hashing), Uploading n%, Finishing, Done, Paused (offline), Failed
(with the reason), Cancelled. Conflict question (name taken) appears once per batch inside the
panel, with "Upload as new version" (default), "Keep both", "Skip". Collapsed: only the header.
Leaving the page while uploading: the browser's "Leave site?" warning.

**Accessibility.** `section aria-label="Uploads"`. Progress bars have labels ("*name*, 45%").
Live updates are throttled: the panel announces only start, each 25% of the whole batch, errors
and the end ("12 files uploaded to Pärna 12"), not each percent. The panel never covers the
focused element: when focus is behind it, it collapses.

### 4.6 Drop zone

**Anatomy.** While files are dragged over a folder the person can change: a 2 px dashed
`--color-primary` border around the list, `--color-primary-soft` background, and the text "Drop
to upload to *folder*". Dragging over a subfolder row targets that folder ("Drop to upload to
*Site photos*").

**States.** View-only folder: the drop zone shows "You can't add files here" with a
`--color-border-control` dashed border, and drop does nothing.

**Accessibility.** The drop zone is a visual aid only; "Upload" is the accessible way (2.5.7).

### 4.7 Item details panel

**Anatomy.** Title (the name, h2) and close button. Tabs: "Details", "Versions" (files). The
Details tab: a large type icon or image preview; the primary actions as a button row: "Download"
(primary) and "Share"; then "Preview", "Rename", "Move", "Move to trash" in a "More" menu on
narrow panels; facts: Kind, Size, Changed (when and who), Created, Where (path, linked), "Your
access" (Can edit, and "from folder *X*" when inherited); Tags; a collapsed "More facts" with ID,
type and current version ID, each with a copy button.

**States.** Loading: skeleton facts. Item gone: "This item is not here any more." View only: only
Download, Preview, Versions, Share (read-only). Changed by agent: the Changed fact shows the agent
mark.

**Accessibility.** `aside` with the item name as the label; opening moves focus to the h2;
closing returns focus to the row. Tabs use the tab pattern (`role="tablist"`, arrow keys). On the
phone the panel is a sheet with the dialog rules (4.12).

### 4.8 Share dialog

**Anatomy** (top to bottom):

1. Title "Share *name*" and close.
2. **Add people**: a combobox for name or email (suggests members and known guests), the role
   menu, "Send an email" checkbox with an optional note (shown when an email is added), "Share"
   button.
3. **People with access**: rows of avatar or initials, name, email (muted), and the role. Order:
   owner first, then direct access, then "from folder *X*" groups. A direct row has a role menu
   whose last item is "Remove access". An inherited row has the role as text and "from folder
   *X*" as a link that opens that folder's share dialog. An email without an account says
   "Invited, not joined yet". "Everyone in *Space*" is one row with its role.
4. **Link**: the switch "Anyone with the link"; when on: role menu, "Ends on" date picker (default
   7 days, at most the space maximum), "Password (optional)", and "Copy link" (only right after
   creation, because the API returns the link only once: "Copy the link now. For safety, Ruum
   shows it only once."). Existing links are rows: role, ends on, "with password", "Turn
   off".
5. **Settings** (managers, folders only): "People who can edit can also share" with the choices
   "Yes", "No", "Same as the folder above (now: yes)".
6. Footer: "Done".

**Role menu.** Each choice has the role and one line: "Can view: see and download", "Can
comment: see and comment", "Can edit: change, upload and rename", "Can manage: also move, delete
and share". Only roles the person may give are listed (at most their own, decision 0001-1).

**States.** Loading lists: skeleton rows. Cannot share: the add form and link controls are
replaced by "You can see who has access, but you can't share this. Ask *owner*." Guests off:
adding an outside email shows the error under the field. Links off: the link section says "Links
are turned off in this space." Saving a role change: the menu shows a spinner and stays; on
success, a toast. Offline: controls disabled.

**Accessibility.** A modal dialog (4.12) labelled by its title. The combobox follows the ARIA
combobox pattern with a listbox of suggestions. Role menus are labelled "Access of *person*".
"Remove access" asks no confirmation but shows a toast with "Undo" (the action is cheap to
reverse); "Turn off" a link asks for confirmation (it cannot be reversed).

### 4.9 Version panel and compare view

**Version panel anatomy.** In the details panel tab "Versions". Groups by day ("Today",
"Yesterday", "5 Nov 2026"). A row: time ("14:00", absolute date in a tooltip), who ("Liis Tamm",
or the agent mark and "Claude Desktop (agent of Liis Tamm)"), size change ("+2 KB"), the version name as a badge if
any, "Restored from 5 Nov, 14:00" if so, "Current" badge on the current version. Row actions:
"Compare" (not on the current), and a "More" menu with "Download", "Name this version", "Restore
this version" (not on the current). Filter chip at the top: "Only named versions". At the end:
the retention note "Versions older than 30 days are removed on your plan. Named versions are kept
forever."

**Compare view anatomy.** Opens wide: a dialog of 90% width on desktop, a full page on the phone.
Header: "Compare versions", two version pickers ("From" and "To", To defaults to the current
one), and "Compare as" (only when more than one way exists: "Text", "Data (JSON)", "XML",
"Image", "File details"). Summary line: "3 added, 1 removed, 2 changed". Body by type:

- **Text**: unified view by default, side by side on wide screens (toggle), line numbers, 3 lines
  of context, collapsed unchanged parts ("Show 40 unchanged lines").
- **Data (JSON, XML)**: a list of changes by path ("plants[3].name: Rosa canina -> Rosa
  rugosa"), nested.
- **Image**: before and after side by side, and a slider overlay; the metadata changes below.
- **Other**: "These files can't be compared line by line." with size and type changes, and
  "Download both".

Footer: "Restore the *From* version" (edit and up), "Close". Truncated results end with
"Only the first 500 changes are shown. Download both versions to see all."

**States.** Loading: version headers shown, body "Comparing..."; Error: "Ruum couldn't compare
these versions." with "Try again"; Same content: "These versions are the same."; One version only
(panel): the empty state.

**Accessibility.** Insertions and deletions use `<ins>` and `<del>` with visually hidden "Added:"
and "Removed:" prefixes; colour is never the only cue (a `+` / `-` gutter). The image slider is a
`role="slider"` with arrow keys and a value text ("Showing 40% of the new version"). Long diffs
are in a scroll region with a label and keyboard scrolling.

### 4.10 Agent cards, agent mark and activity

**Agent mark.** The 16 px clay glyph plus the text "*Agent* for *Person*". Used in: the list's
Changed column, version rows, the details Changed fact, activity rows, the audit log, toasts
about agent changes. Never shown for a person's own changes. The screen-reader text is "*Agent*,
an agent for *Person*".

**Agents page** shows agent cards in a grid (one column on the phone). A card: the agent mark
and name (h3, a link), status badge (Active, Paused, Access ended), the access summary ("Can edit
in Clients/Pärn, until 4 Feb 2027"), "Last used 2 minutes ago" or "Not used yet", and for admins
"Owner: Jonas Kask". The first card slot, when the person has no agents, is the empty state.

**Agent page.** Header: name, status, "Pause" / "Resume" (secondary), "Remove" (in a menu).
Sections: **Access** (the summary and "Change access", which opens the access form), **Keys**
(rows with the key prefix in mono, name, ends on, last used, "Turn off"; "New key"; the note
"Apps that connect with sign-in (OAuth) don't need a key."), **Activity**.

**Activity.** Changes grouped into runs ("Today, 14:02-14:09, 7 changes"); each change a row:
action verb, item name as a link to the item, folder in muted text, time. A run header has "Undo
these changes". At the top: "Undo changes..." for a time range (a dialog with From and To).
Every undo starts with a dry run and a confirm dialog that lists what will change and what stays.

**States.** Paused: card and page show the Paused badge and a note "Paused. Its keys and
connections don't work until you resume it." Access ended: "Access ended on 4 Feb 2027. Change
access to give it a new end date." Owner left the space: "Paused because *owner* left the space."
No activity: "*Agent* hasn't changed anything yet."

**Accessibility.** Cards are `article` elements in a list; the whole card is not one big link
(the name is the link, so the other text stays readable). Status badges have text, not only
colour. The undo dialog lists items as a real list, with the count in the title.

### 4.11 Empty states

**Anatomy.** Centred in the region, max 420 px wide: a 48 px line illustration in
`--color-text-faint` (simple, warm: an open folder, a tray, a link, a clock, a tool; no people,
no robots), a title (h2 or h3, `--text-lg`), one or two sentences in `--color-text-muted`, and one
primary action, sometimes one secondary.

**Rules.** An empty state says what the place is for and the one next step. It never blames
("You haven't..."). View-only places have no action. A search with no results suggests a next
step (check spelling, search all of the space). The copy is in `copy.md`, section 5.

**Accessibility.** The illustration is decorative (`aria-hidden="true"`). The title is a heading
at the right level of the page outline.

### 4.12 Dialogs

**Anatomy.** Native `<dialog>` with `showModal()`: title (h2), close button (icon, "Close"),
body, footer with buttons at the right: the safe choice first ("Cancel"), the main action last.
Widths: 480 px (confirm, forms), 720 px (share), 90% (compare). On the phone: full-width sheet
from the bottom, max 90% height, with the footer fixed.

**Confirm dialogs** (destructive or irreversible) say what happens, to what, and how to get it
back, in the body; the button repeats the verb and the object ("Delete 12 items for good", not
"OK" or "Yes").

| Action | Confirm? | Button style |
|---|---|---|
| Move to trash | No; toast with "Undo" | Secondary |
| Remove access | No; toast with "Undo" | Secondary (menu item) |
| Restore a version | Yes (it changes the current version) | Primary |
| Turn off a link, turn off a key | Yes (cannot be reversed) | Danger |
| Delete for good, Empty trash | Yes, with the count | Danger |
| Remove a member | Yes, with the effects | Danger |
| Undo agent changes | Yes, after the dry run | Primary |
| Pause an agent | Yes | Secondary |
| Run a query that deletes or replaces data (a person, DB-4) | Yes, with the row count when known | Danger |
| Give an agent access (4.23) | The prompt is the confirm | Primary |
| End an agent's access now | No; toast | Quiet (row action) |
| Approve or deny a request (4.24) | The request dialog is the confirm | Primary / Secondary |
| Restore a database as a new database | No (the dialog is the confirm) | Primary |
| Replace the data in a database | Yes (it can be undone from a restore point) | Primary |

**States.** Busy: the main button shows a spinner and "Deleting..." and all buttons are
disabled; Escape still closes when nothing is in flight. Error: an error note above the footer;
the dialog stays open with the input kept.

**Accessibility.** Focus moves to the first field, or to the safe button in a confirm dialog;
Tab stays inside; Escape closes; focus returns to the opener. `aria-labelledby` the title and
`aria-describedby` the body text of confirm dialogs.

### 4.13 Toasts

**Anatomy.** A stack at the bottom right (above the upload panel), or the top of the bottom bar
on the phone. Each toast: an icon (success, info, error), one line of text, an optional action
("Undo", "Show", "Try again"), and a close button. `--radius-lg`, `--shadow-2`,
`--color-surface`.

**Rules.** Toasts confirm an action that the person just did. They never carry the only copy of
important information (that goes inline). At most 3 visible; the oldest leaves first. Success
toasts stay 6 s, toasts with an action stay 10 s, error toasts stay until closed. Hover or focus
pauses the timer (WCAG 2.2.1).

**Accessibility.** A single live region `role="status"` (polite) for success and info, and one
`role="alert"` for errors. The action button is reachable with the keyboard; the shortcut F6 or
"Alt+T" moves focus to the newest toast. A toast never moves focus by itself.

### 4.14 Inline notes and errors

**Anatomy.** A block with a 3 px left border, a soft background, an icon and the text, with an
optional action: Info (`--color-primary-soft`), Success (`--color-success-soft`), Warning
(`--color-warning-soft`), Error (`--color-danger-soft`). The error note is for a region that
failed to load or a form that failed to save; it names the problem and the next step, and shows
"Try again" when that can help. A small "Details" disclosure shows the error code and request ID
for support, in mono.

**Accessibility.** Error notes that appear after an action use `role="alert"`; notes present
on load have no role. Never show the raw API message to people (see `copy.md`, section 4).

### 4.15 Forms

**Anatomy.** Label above the field (never only a placeholder), optional helper text under the
label, the field, the error text under the field. Required fields are the default; optional
fields say "(optional)" in the label. Field width matches the expected input (a date is short,
an email is long). Buttons at the end of the form, the main one last.

**Controls.** Text input, email, password (with "Show"), date picker (native `type="date"`, with
the format shown as helper text), select (native for simple lists; a menu button when items need
two lines), checkbox, radio group (a `fieldset` with a `legend`), switch (a `button
role="switch"` with an on/off label), combobox (people, tags, folders).

**States.** Default border `--color-border-control`; focus: the focus ring; error: border
`--color-danger`, an error icon and text under the field, `aria-invalid="true"` and
`aria-describedby` to the text; disabled: 50% opacity of the border, text still 4.5:1, and the
reason as helper text; read-only: no border, the value as text.

**Validation.** On submit first, then live after the first error. The first field with an error
gets focus. Names: trim spaces; show the server's `name_taken` under the field. Never clear what
the person typed after an error.

**Accessibility.** `label for` on every field; groups in `fieldset`; `autocomplete` on email and
name fields; no time limits on forms.

### 4.16 Buttons

| Kind | Look | Use |
|---|---|---|
| Primary | `--color-primary` fill, `--color-on-primary` text | One per view: Upload, Share, Save, Allow |
| Secondary | `--color-surface`, `--color-border-control` border | Everything else |
| Quiet | No border, `--color-link` text | Low-weight actions in rows and toolbars |
| Danger | `--color-danger` fill, `--color-on-danger` text | Only the confirm button of an irreversible action |
| Icon | 36 px (44 px touch), tooltip and `aria-label` | Close, more actions, copy |

**States.** Hover: primary darkens to `--color-primary-hover`, secondary gets `--color-sunken`;
active: 1 px down shift is not used (calm), the colour darkens one more step; focus: the ring;
disabled: 50% opacity with the reason nearby; busy: a spinner replaces the icon, the label stays,
`aria-busy="true"`.

"Move to trash" is a secondary button (it can be undone), not a danger button. Only the confirm
button of an irreversible action is `danger`.

### 4.17 Badges and tags

Badges (`--text-xs`, `--radius-sm`): Current, version names, Active / Paused / Access ended,
Owner / Admin / Member / Guest, "Invited". Tags: the tag name with its colour dot, removable with
an icon button "Remove tag *name*". Neither is ever only colour.

### 4.18 Offline banner and session dialog

Offline banner: `--color-warning-soft`, the text "You're offline. Changes wait until you're back."
and no close button; it leaves by itself with "Back online" for 3 s. It uses `role="status"`.
Session ended dialog: "You've been signed out" / "Sign in again to keep working. What you typed
stays here." / "Sign in again" (primary).

## 4b. Components of step 2 (databases)

Added for step 2 (`databases/`). They use the tokens of section 2 only; no new colour token is
needed. Two rules hold for all of them:

- **Markers never look like data.** A stand-in or a hidden value is a chip with a dashed
  `--color-border-control` border, an icon and words, on `--color-sunken`. It is never plain
  text, never asterisks, never a colour alone.
- **The agent colour stays for agents.** Chips, privacy badges and write mode do not use
  `--color-agent`; the agent mark keeps its meaning (an agent did or asked this).

### 4.19 Database item and status badge

**In Files.** A row with the "database" outline icon (Lucide `database`), the name, a status
badge when the state is not Ready, the size (the data size from `size_bytes`), Changed. Quiet
links in the name cell when they apply: "*1* request", "*3* columns to confirm", "*2* branches".
A click on the name opens the database page (not the details panel). The details panel of a
database shows Kind, Status, Size, Created, Where, "Your access", and the primary button "Open
database".

**Status badge** (4.17 badge rules, text always present):

| State | Badge | Style |
|---|---|---|
| `provisioning` | Setting up | neutral, with a 12 px spinner (none with reduced motion) |
| `ready` | Ready | success text; shown on the database page, not in Files rows |
| `updating` | Updating (or Restoring during a restore) | neutral, spinner |
| `degraded` | Needs attention | warning |
| `suspended` | Paused | warning |
| `failed` | Didn't start | danger |

**Accessibility.** The row reads "garden, database, Setting up". On the database page the badge
sits in a `role="status"` region, so a change ("Ready") is announced once, politely.

### 4.20 Query editor

**Anatomy.** A mode row: the radio pair "Read only" / "Can change data" (the second only with
Can edit). The editor: a code area in `--font-mono`, `--text-md`, line numbers in
`--color-text-faint`, `--color-sunken` background, `--radius-md`, at least 6 lines high, grows to
20 lines, then scrolls. Below: the "Values" row with one labelled field per placeholder found
(`$1`, `$2` for Postgres; `?1`, `?2` for SQLite; the dialect label "Postgres" or "SQLite" sits at
the editor's top right); the helper "Put values here, not in the query. Ruum never stores them."
Footer: "Copy" menu (enabled when there is a result) and the primary button "Run query" with the
shortcut hint "Cmd+Enter" (Ctrl+Enter on other systems).

**Behaviour.** Cmd/Ctrl+Enter runs. Tab inserts two spaces; Escape then Tab leaves the editor
(so Tab is not a trap, WCAG 2.1.2); the helper text under the editor says so. Syntax colours are
optional and must pass 4.5:1 on `--color-sunken`; keywords may be medium weight instead of a
colour. A table or column clicked in the tables list inserts its name at the cursor (quoted when
needed). One statement per run: the editor does not split statements.

**States.** Default; focus (the ring around the whole editor); running (button "Running...",
`aria-busy`, a "Stop" link after 2 s); write mode (the frame is 2 px `--color-warning`, a label
"Can change data" with a warning icon above the editor, the button reads "Run and change data");
error (the error note under the editor, the line of the error marked with a `--color-danger`
gutter mark and the visually hidden text "Error on line 2"); disabled (offline, database not
ready: the reason under the button).

**Accessibility.** The code area is a `textarea` (or an editor with `role="textbox"
aria-multiline="true"`) labelled "Query"; the values fields are labelled "Value for $1". Results
are announced once when they arrive: "2 rows" or the error title, through `role="status"`
(errors: `role="alert"`).

### 4.21 Result grid and hidden-value chips

**Anatomy.** A `<table>` in a scroll region (`tabindex="0"`, labelled "Result", both directions
inside the region, never the page). The header row per column: the name (`--text-md`, 600), the
type in `--text-sm` muted mono, and a privacy badge for Personal and Secret ("Personal",
"Secret"; Normal shows none). A column whose class is not confirmed adds "not confirmed" to the
badge with a dashed outline. A sticky header and a sticky first column on wide results. Footer
under the region: "*12* rows · *34* ms · Read only" or "Changed *3* rows · *12* ms". Above the
region, when it applies: the cut note (4.14 info) and, on a masked branch, the masked note.

**Cells.**

| Value | Shown as |
|---|---|
| text, numbers, dates | as text; numbers right-aligned with tabular figures; long text cut at 200 characters with "..." and the whole value in a popover on Enter or click |
| `null` | "NULL" in `--color-text-faint`, italic |
| boolean | "true" / "false" |
| array, object | compact JSON in mono, cut, popover for the whole value |
| `LiteralValue` (`$ruum: literal`) | its `value` as JSON, like any object; never as a marker |
| `HiddenValue` (`class: secret`) | the **hidden chip**: lock icon + "hidden: secret" |
| `HiddenValue` (`class: personal`) | the hidden chip: lock icon + "hidden: personal" |
| `Pseudonym` | the **stand-in chip**: person icon + "person 8f3a91" (the first 6 hex characters after `p_`) |
| a `p_` + 12 hex string in a Personal column of a masked branch (DB-9) | the stand-in chip, as above |

**Chip.** Height 24 px, `--radius-sm`, `--color-sunken` background, 1 px dashed
`--color-border-control` border, a 14 px icon and `--text-sm` text in `--color-text-muted`; the
id part in mono. Tooltip on hover and focus: hidden chip "Hidden. Only people who can manage
*garden* see secret values." (for agents' views in the log and requests: "Hidden from agents.");
stand-in chip "A stand-in for a personal value: *p_8f3a91c2d4e5*. The same value always gets the
same stand-in." The chip is focusable only when the cell is (one tab stop for the grid, arrow
keys inside, as the grid pattern).

**Copy and export.** "Copy as CSV", "Copy as table", "Download CSV". Markers are written as
`[hidden: secret]`, `[hidden: personal]` and `[person 8f3a91]` (DB-7). The header row is
included. Toast "Copied *12* rows."

**States.** Loading (old result dimmed, `aria-busy`), no rows ("No rows match." under the
header), cut, error (no grid, the editor note), masked branch note.

**Accessibility.** The table has a caption ("Result of the query, 12 rows") and `scope="col"` on
headers. Screen-reader text of the chips: "Hidden secret value" and "Stand-in for a personal
value, person 8 f 3 a 9 1". The privacy badge is text, so a screen reader reads "owner, text,
Personal".

### 4.22 Classification review list

Used in the request dialog of a migration (4.24), in "Review columns" (Columns tab), and later in
the publish approval of steps 3 and 4 (design 8.5 and 9.6).

**Anatomy.** An intro line with the three classes in plain words (copy.md 3.8). Then one group
per table (h3 with the table name), and per column one row:

- the column name (mono) and type (muted);
- a radio group (`fieldset`, legend = "Privacy of *table.column*"): Normal, Personal, Secret, as
  a segmented control of three buttons-as-radios, 36 px high (44 px on touch);
- under it, the suggestion: "Suggested by *Claude Desktop*: *looks like an email address*." with
  the agent mark when an agent suggested it, or "Suggested by Ruum: *the name looks like an email
  column*." for the rule-based suggester; none when there is no suggestion (then Personal is
  preselected and the line says "No suggestion. Personal until you choose.");
- for people who can manage, the quiet link "Show 3 example values", which runs a read-only
  `SELECT <column> FROM <table> LIMIT 3` and shows the values inline (never stored).

Preselected: the suggestion; else Personal. Choosing a class **lower** than the suggestion (or
than Personal when there is none) shows a warning note under the row: "Agents will see the real
values of *phone*." (to Normal) or "Agents will see stand-ins for *token* instead of hiding it."
(Secret to Personal). Choosing higher shows nothing. A column that was reset by a rename shows
"Renamed from *mail* on 6 Nov. Choose again."

The submit button counts: "Confirm *4* columns" or, in a request, "Approve and confirm *4*
columns". It is always enabled: every row has a value.

**States.** Loading (skeleton rows), all confirmed ("All columns are confirmed."), saving (the
button busy), error (`precondition_failed`: the list reloads with a note), read only (people
without Can manage: the radios are replaced by the suggested class as text and "Ask *owner* to
confirm these.").

**Accessibility.** Each row is a `fieldset` with a `legend`; arrow keys move inside the radio
group; the warning note is linked with `aria-describedby` and announced politely when it
appears. The count in the button updates without moving focus.

### 4.23 Write-grant prompt

The dialog to give an agent extra access to one database for a short time (`write_live` or
`read_personal`). Opened by "Give access" or by a link from an agent (`?give=`).

**Anatomy** (480 px dialog; a sheet on the phone):

1. Title "Give an agent access to *garden*".
2. When opened from a link: a quoted block with the agent mark: "Claude Code asks for: Change
   data, 1 hour." and, when the link carries one, "Reason from Claude Code (Ruum didn't check
   this): *fix the 12 plants with a wrong family*." The reason is plain text, never rendered as
   a link or markup.
3. "Agent": a select of the person's own agents that can reach this database, with the agent
   mark and the access line ("Can edit in Projects/Garden planner").
4. "Access": two large radio cards: **Change data** (pencil icon): "Insert, update and delete
   rows in *garden*. Queries that delete or replace data still ask you." **See personal data**
   (eye icon): "See real names, emails and other personal values instead of stand-ins. Secret
   values stay hidden."
5. "For": a segmented control 15 min / 1 hour / 4 hours / 8 hours / 24 hours ("See personal
   data": up to 8 hours; longer choices are disabled with the reason).
6. "Reason (optional)": a text field, prefilled from the link.
7. The summary (an info note): "*Claude Code* can change data in *garden* until *15:42* today.
   Ruum records a restore point first, so you can go back to *14:42*."
8. Footer: "Cancel", primary "Give access for 1 hour" (the button names the time).

**Rules.** Defaults: Change data (or the kind from the link), 1 hour. Values from a link are
capped to the contract limits and the cap is said in words. An agent that is paused, whose access
is Can view only (for Change data), or that belongs to another person cannot be chosen; the
reason is shown. Nothing is given without the button.

**States.** Loading agents (skeleton select), busy (button), error (`permission_denied`,
`validation_failed`: a note above the footer), no own agents ("You have no agent that can reach
*garden*." with "Connect an agent").

**Grant rows** (Agent access tab, "Access now" and "Earlier"): agent mark and name, the kind
("Change data" / "See personal data"), "until 15:42 (58 min left)" (updated each minute, not each
second; a visually hidden text gives the end time only), "Restore point 14:42" link, "Used *4*
times" when known, "End now" (quiet). Ended rows say how they ended: "Ended at 15:42", "Ended by
Jonas Kask at 15:10", "Claude Code gave it back at 15:10", and offer "Go back to before this
access" (opens 4.25 with the grant's restore point).

**Accessibility.** The radio cards are a `fieldset` of radios with their descriptions linked by
`aria-describedby`. The countdown is not a live region.

### 4.24 Request card and request dialog

**Card** (Overview and Agent access, "Waiting for you"): `--color-surface`, `--radius-lg`, a
3 px left edge in `--color-warning`, the agent mark and the request in one line ("*Claude Code*
for *Jonas* wants to delete rows in *plants*"), a second line "About *1,240* rows. Asked *3 min*
ago. Ends in *23 h 57 min*.", and "Review" (secondary). Decided requests leave the card list and
move to "Requests, decided" (a plain list).

**Dialog** (screens.md S5): title "Review the change from *Agent*", the by-line with the agent
mark, the change in words (from `reasons`: copy.md 3.8), the statement in mono in a scroll box
with the values as chips (parameters as the requester saw them: markers), "About *n* rows"
(`estimated_rows`; "Ruum can't count the rows for this change." when null), the effect line,
and for a migration the classification review list (4.22). Footer: "Deny" (secondary; opens an
optional reason field in place) and the primary "Approve" or "Approve and confirm *n* columns".

**Rules.** The statement is shown exactly as the agent sent it, in mono, never reformatted.
Statement text and comments in it are untrusted: they are shown as text, never as markup or
links. "Approve" says in the dialog that it lets the agent run exactly this query once, in the
next 15 minutes, and that write access is still needed.

**States.** Pending, ended ("This request has ended. *Agent* can ask again."), decided by someone
else, read only (cannot decide: "Only *Liis Tamm* or people who can manage *Plant list* can
decide this."), busy, error.

**Accessibility.** The card is an `article` with the request as its heading. The statement box is
a labelled scroll region ("The query"). Focus goes to "Deny" first (the safe choice, 4.12).

### 4.25 Restore picker

**Anatomy** (480 px dialog; a sheet on the phone):

1. Title "Restore *tracker* to an earlier time".
2. The window: "You can go back to any time since *30 Oct, 11:02*."
3. "Go back to": a radio pair. **A time**: a native date field and a time field (24 h, minutes),
   with the time zone as helper text ("Tallinn time (UTC+2)"), limited to the window; under it,
   the nearest restore point in words ("That's 20 minutes before *Before import* (11:20)").
   **A restore point**: a list of the points (time, label or reason, who), one selectable.
4. "Restore how": two radio cards. **As a new database next to *tracker* (recommended)** with a
   "Name" field (default "*tracker* (restored *6 Nov 11:00*)"). **Replace the data in
   *tracker***: "*tracker* answers no queries while Ruum restores it, usually a few minutes.
   Ruum records a restore point of the data now, so you can undo this."
5. Footer: "Cancel", primary "Restore" (for a new database) or "Replace the data..." (opens the
   confirm of 4.12).

**Rules.** Default "A time" when opened from the button, "A restore point" with the point chosen
when opened from a row or from "Go back to before this access". Default "As a new database".
Times are sent as RFC 3339 with the offset. A guest without the policy sees only "Replace the
data" (and only with Can manage).

**States.** No backup yet (the dialog does not open; the button is disabled with the reason),
time outside the window (field error), restore running ("A restore of *tracker* is already
running."), busy, error.

**Accessibility.** The date and time fields have labels and the format as helper text; the
radio cards are a `fieldset`; the window line is the dialog description.

### 4.26 Query log list

**Anatomy.** A filter row (three menu buttons: "Who", "What", "Outcome"; the state is in the
URL), the note "Ruum never stores the values in queries. They show as value 1, value 2.", and a
list of entries. An entry: time; who (agent mark and "*Agent* for *Person*", or the person); the
mode badge "Read" or "Changed data"; the statement on one line in mono, cut with "..." (the
stored `$L1`, `$L2` render as the chips "value 1", "value 2": same chip style as 4.21 with a
"#" icon, so they are never read as numbers); the outcome ("*12* rows", "Changed *12* rows",
"Failed: *the code in words*", "Stopped: took too long", "Cancelled", "Waiting for you"); the
time taken; "with write access" when a grant was used. Failed entries have a danger icon and the
word "Failed" (never colour alone).

**Details sheet.** The whole statement (wrapped), the values as stand-in chips (stored as
pseudonyms) or "none", outcome and error code ("Details": the code and the SQLSTATE), rows, time,
the grant (link) and the request (link) used, the query ID with a copy button.

**States.** Loading, empty, filter with no match, error, hidden without the right (journeys J14).

**Accessibility.** The list is a `ul` of `article` entries, each with a heading "*15:12*,
*Claude Code for Jonas*, Changed data". The value chips read "value 1".

## 5. Screen checklist for UI task cards

A UI task card is done only when its screenshots at 1280 px and 390 px, light and dark, show:

- the tokens of section 2 only (no raw colours in the diff);
- each state of the journey table (loading, empty, error, no permission, offline);
- the copy of `copy.md` word for word;
- no axe violations, and the keyboard path of the journey works;
- the agent mark wherever an agent did something.

## 6. The design system in code

Design 12.5 (decided 2026-11-07, provisional). The look of Ruum is defined in one place, so it
can change without touching screens. Built on the branch `w/design-system`.

### 6.1 Tokens

- **Source:** `web/src/design/tokens/`, in the W3C Design Tokens format (2025.10: colours as
  `{colorSpace, components, hex}`, dimensions and durations as `{value, unit}`).
  - `base.tokens.json`: type, spacing, radius, border widths, sizes, motion, opacity, layers,
    breakpoints. The same in every theme.
  - `color.light.tokens.json` and `color.dark.tokens.json`: the colour sets and the shadows. Both
    files have the same names.
- **Theme packages** (design 12.6): `web/src/design/themes/<id>/` gives new values to these
  tokens (never new tokens) and adds the wordmark, favicon, app icons and fonts. The default is
  `graphite`. `web/scripts/check-theme.mjs` validates a package (schema, file types and sizes,
  token types, SVG shapes, and every contrast pair) and refuses a failing one with the failing
  pairs listed. A new brand is a new package, with no change to components or screens. The
  format is in `web/src/design/themes/README.md`.
- **Build:** `npm run tokens` runs Style Dictionary (`web/scripts/build-tokens.mjs`) over the core
  tokens with the default theme over them, and writes `web/src/design/generated/tokens.css` (the
  theme's `@font-face` rules; light on `:root`; dark under `prefers-color-scheme` unless
  `data-theme="light"`, and under `data-theme="dark"`), `tokens.ts` (the values for code, the
  theme's name, and `cssVar()`), `brand.ts` (the wordmark, drawn by `Brand`) and the favicon,
  app icons and web manifest in `web/public/`.
- **Committed, not built:** the generated files are committed, like `src/api/schema.d.ts`, so
  Vitest, the type check, Vite and Ladle need no build step. `npm run lint` runs
  `build-tokens.mjs --check` and fails when they are out of date (CI runs `npm run lint`).

### 6.2 Components

One library in `web/src/ui/`, exported from `web/src/ui/index.ts`. Each folder has the
component, its CSS (only tokens) and its Ladle stories. Every state and the accessibility rules of
sections 3 and 4 are inside the component, so screens cannot forget them.

| Folder | Components |
|---|---|
| `layout` | `Stack`, `Row`, `Grow`, `Actions`, `Toolbar`, `Columns`, `Indent`, `Page`, `PageBody`, `Card` |
| `text` | `Text` (tone, size, weight, mono, truncate, numeric), `VisuallyHidden`, `Code`, `ProseList` |
| `button` | `Button` (primary, secondary, quiet, danger, link; small; busy), `ButtonLink`, `ButtonAnchor`, `IconButton` |
| `field` | `Field` (label, hint, error), `TextInput`, `Select`, `TextArea`, `Checkbox`, `BareCheckbox`, `Radio`, `Fieldset`, `Switch`, `FileInput`, `CopyField`, `SecretBox` |
| `dialog` | `Dialog` (sm, md, full; sheet on the phone), `ConfirmDialog`, `ConfirmButton` |
| `feedback` | `Note`, `ErrorNote`, `Notice`, `Loading`, `Loaded`, `Spinner`, `Skeleton`, `EmptyState`, `Banner`, `ProgressBar`, `Meter` |
| `badge` | `Badge`, `Tag`, `TagList`, `Chip` (hidden, stand-in, value markers of 4b), `nearestTagColor` |
| `list` | `List`, `ListRow`, `ListRowMain`, `ListEmpty`, `Facts`, `Fact`, `Picker`, `PickerItem`, `LoadMore` |
| `table` | `Table`, `THead`, `TBody`, `Tr` (selected, drop target), `Th`, `Td`, `SortHeader`, `NameCell` |
| `nav` | `Breadcrumbs`, `Crumb`, `CurrentCrumb`, `Tabs` |
| `menu` | `Menu` (menu button with arrow keys, Home, End, type-ahead, Escape) |
| `toast` | `ToastProvider`, `useToast`, `ToastRegion`, `ToastView` |
| `icon` | `Icon` (20 outline icons), `FolderIcon`, `FileIcon`, `KindIcon`, `AgentMark` (the key mark of 2.6) |
| `agent` | `AgentName` (mark, name, screen-reader text) |
| `shell` | `SkipLink`, `TopBar`, `Brand`, `TopBarItem`, `SearchForm`, `MenuToggle`, `AppBody`, `NavRail`, `NavList`, `NavItem`, `NavButton`, `AppMain`, `SplitView`, `SelectionBar`, `DropZone`, `FloatingPanel` |
| `diff` | `DiffView`, `DiffList`, `DiffChange` |

`web/src/ui/global.css` imports the tokens and sets the element defaults (body, headings, links,
focus ring, reduced motion). Screens (`src/app`, `src/pages`, `src/components`) compose these
components and hold no styling of their own.

### 6.3 Lint

`npm run lint` (CI job "web build and generated client") runs:

- **oxlint**, as before.
- **stylelint** (`web/stylelint.config.mjs`) on every CSS file outside the generated tokens: no
  hex, named or functional colour (`rgb()`, `hsl()`, ...); no length or time unit (`px`, `rem`,
  `em`, `ch`, `ms`, `s`, ...), except `px` in media queries (the breakpoints); colour, `font*`,
  `line-height`, `box-shadow`, `z-index`, `opacity`, `border-radius`, `transition` and animation
  times only through a token (`declaration-strict-value`); no `!important`; no ID selectors.
- **`web/scripts/check-ui-rules.mjs`** (TypeScript AST): style files only in `src/ui/` and
  `src/design/`; no `style` attribute anywhere; no `className` and no raw `button`, `input`,
  `select`, `textarea`, table elements, `dialog`, `progress`, `meter`, `label`, `fieldset` or
  `legend` in screens; a Ladle story file in every `src/ui/` folder.
- **`build-tokens.mjs --check`**: the generated token files are up to date.

- **No literal UI text** (design 12.4, the same script): in `src/app`, `src/pages`,
  `src/components` and `src/ui` (not stories or tests) words come from the message catalogue
  (`t()`, `docs/ux/i18n.md`). The rule finds JSX text with a letter, and string or template
  literals with a letter as a text prop (`label`, `title`, `aria-label`, `placeholder`, ...), as
  a JSX child (also inside `?:` and `&&`), as a text property of an object or as the text of
  `toast()` and `setNotice()`.

### 6.4 Catalogue

**Ladle** shows every component in every state: `npm run ladle` (serve) and `npm run ladle:build`
(to `web/build-ladle/`, not committed). The theme button (key `d`) switches light and dark through
`data-theme`, the same switch as the Account page; the width addon has phone (390 px) and desktop
(1280 px); the a11y panel runs axe. Stories are `web/src/ui/<folder>/*.stories.tsx`.

`web/src/ui/catalogue.test.tsx` (Vitest) renders every story and runs axe (WCAG 2.2 A and AA,
without colour contrast, which jsdom cannot compute: `contrast.mjs` checks the token pairs and the
visual tests check the screens). Add `&locale=en-XA` (the pseudo-locale) or `&locale=et` to the
address of a story to see it in that locale (12.4). Per-component visual snapshots are still to
do.

