# Design system

Status: `provisional`. The tokens and components of the Ruum web UI for step 1. The target is
WCAG 2.2 level AA everywhere. The names of things follow `ia.md`; the strings follow `copy.md`.

## 1. Identity

Ruum keeps people's work and lets agents touch it. The look must say **calm, trustworthy, a
little warm**.

- **Calm.** Few colours, much space, one primary action per view, no motion that is not feedback.
- **Trustworthy.** Plain words, the truth about state (saved, uploading, offline), every
  destructive action explains how to get things back, agents are always visible.
- **A little warm.** Warm neutrals (paper, stone) instead of blue-grey; a deep spruce green as the
  primary colour; a clay-ochre for agents; rounded but not bubbly corners; sentence case; a human
  voice in the copy.

**Not the generic AI look.** No purple-to-blue gradients, no sparkle or magic-wand icons for
agents, no glass blur, no glowing borders, no dark-mode-first neon, no emoji in the UI, no
"AI-powered" badges. An agent is a worker with a name, not magic: its mark is a small solid
key glyph in clay, the same everywhere.

**Not a copy of another product.** The current UI uses the GitHub Primer palette
(`web/src/index.css`). Replace it with the tokens below.

## 2. Tokens

Tokens are CSS custom properties on `:root`. Components use only tokens, never raw values. Dark
mode follows `prefers-color-scheme`; a person can force light or dark on the Account page
(`data-theme` on `<html>`). Every text pair below is checked by `docs/ux/tools/contrast.mjs`
(WCAG 2.2: 4.5:1 for text, 3:1 for control borders, icons and the focus ring). All pairs pass.

### 2.1 Colour

| Token | Light | Dark | Use |
|---|---|---|---|
| `--color-canvas` | `#FAF8F4` | `#161513` | Page background (warm paper) |
| `--color-surface` | `#FFFFFF` | `#1E1C19` | Cards, panels, dialogs, the list |
| `--color-sunken` | `#F2EFE9` | `#121110` | Top bar, rail, code, table header, compare background |
| `--color-border` | `#E0DBD2` | `#37332D` | Dividers and card edges (decorative, no contrast need) |
| `--color-border-control` | `#8A8377` | `#7D766A` | Borders of inputs, selects, checkboxes (3:1) |
| `--color-text` | `#24211C` | `#EDE9E2` | Body text, headings |
| `--color-text-muted` | `#5C564C` | `#ABA497` | Secondary text, metadata |
| `--color-text-faint` | `#736C61` | `#948D81` | Placeholders, timestamps (still 4.5:1 on surface) |
| `--color-primary` | `#1D5C57` | `#72C2B9` | Primary buttons, selected nav, focus ring (spruce) |
| `--color-primary-hover` | `#164844` | `#8FD1C9` | Hover and pressed primary |
| `--color-on-primary` | `#FFFFFF` | `#0E1D1C` | Text on primary |
| `--color-primary-soft` | `#E2EEEB` | `#1E3431` | Info notes, selected rows, drop zone |
| `--color-link` | `#1D5C57` | `#82CBC2` | Links (always underlined in running text) |
| `--color-focus` | `#1D5C57` | `#72C2B9` | Focus ring |
| `--color-selected` | `#E2EEEB` | `#1E3431` | Selected row background |
| `--color-danger` | `#A3301C` | `#F08B74` | Errors, destructive buttons (brick) |
| `--color-on-danger` | `#FFFFFF` | `#1A0C08` | Text on danger buttons |
| `--color-danger-soft` | `#F7E6E1` | `#3B201A` | Error note background |
| `--color-success` | `#2B6439` | `#82C291` | Success text and icons (moss) |
| `--color-success-soft` | `#E4EFE5` | `#1D2E21` | Success note background |
| `--color-warning` | `#7F5300` | `#E2B455` | Warnings (unverified app, plan nearly full) |
| `--color-warning-soft` | `#FAEFD8` | `#33280F` | Warning note background |
| `--color-agent` | `#8A4718` | `#E9A672` | The agent mark and agent names (clay) |
| `--color-agent-soft` | `#F5E7DA` | `#3A2A1D` | Agent badge background, agent rows in Activity |

Rules:

- **Colour never carries meaning alone.** Errors have an icon and words; diff insertions have a
  `+` and the word "Added" for screen readers; the agent mark is a glyph and a name.
- **Tag colours** come from a fixed set of 8 (spruce, moss, clay, brick, ochre, slate, plum, sand)
  with a light and a dark value each, used as a 3 px left edge and a dot, never as text colour.
  The API's free `color` value maps to the nearest of the 8.
- **Diff colours**: insertions on `--color-success-soft` with `--color-success` text, deletions
  on `--color-danger-soft` with `--color-danger` text and a strike-through.

### 2.2 Type

The system font stack: no font download (fast on slow links, nothing sent to a font host, which
matters for EU customers), and it reads well in every language. Warmth comes from colour, space
and copy, not from a display face.

```css
--font-sans: system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", sans-serif;
--font-mono: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
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
badge). Numbers in tables use `font-variant-numeric: tabular-nums`. Line length of running text at
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
| `--radius-sm` | 4 px | Badges, tags, checkboxes |
| `--radius-md` | 8 px | Buttons, inputs, rows on hover |
| `--radius-lg` | 12 px | Cards, panels, dialogs, toasts |
| `--radius-full` | 999 px | Avatars and the space switcher pill |
| `--shadow-1` | `0 1px 2px rgb(36 33 28 / 0.08)` | Cards (light only; dark uses a border) |
| `--shadow-2` | `0 8px 24px rgb(36 33 28 / 0.14)` | Dialogs, menus, toasts, the upload panel |
| `--border-1` | `1px solid var(--color-border)` | Dividers |

### 2.5 Motion

| Token | Value | Use |
|---|---|---|
| `--motion-fast` | 120 ms `cubic-bezier(0.2, 0, 0, 1)` | Hover, press, focus |
| `--motion-base` | 200 ms `cubic-bezier(0.2, 0, 0, 1)` | Panels, dialogs, toasts in |
| `--motion-exit` | 150 ms `cubic-bezier(0.4, 0, 1, 1)` | Things that leave |

Rules: motion only as feedback (something opened, moved, finished). No looping animation except
progress. With `prefers-reduced-motion: reduce`, all transitions become opacity fades of 0 ms to
120 ms, and progress bars stay but do not shimmer. A new row highlights with
`--color-primary-soft` that fades over 2 s (no movement).

### 2.6 Icons

One outline icon set at 20 px with a 1.5 px stroke (Lucide, MIT licence, as inline SVG; a new
dependency needs approval, else draw the 20 icons we need). Icons sit beside a text label;
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
    for J1-J6 in Playwright.

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
"Shared" mark icon and the tag dots), Changed (relative time and who: "2 h ago, Claude for Liis"
with the agent mark), Size (files only), a row menu button ("More actions for *name*"). Folders
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
or the agent mark and "Claude for Liis"), size change ("+2 KB"), the version name as a badge if
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

"Move to trash" is a secondary button (it can be undone), not a danger button. The current CSS
rule `.actions button.danger:last-child` fills every last danger button red; remove it.

### 4.17 Badges and tags

Badges (`--text-xs`, `--radius-sm`): Current, version names, Active / Paused / Access ended,
Owner / Admin / Member / Guest, "Invited". Tags: the tag name with its colour dot, removable with
an icon button "Remove tag *name*". Neither is ever only colour.

### 4.18 Offline banner and session dialog

Offline banner: `--color-warning-soft`, the text "You're offline. Changes wait until you're back."
and no close button; it leaves by itself with "Back online" for 3 s. It uses `role="status"`.
Session ended dialog: "You've been signed out" / "Sign in again to keep working. What you typed
stays here." / "Sign in again" (primary).

## 5. Screen checklist for UI task cards

A UI task card is done only when its screenshots at 1280 px and 390 px, light and dark, show:

- the tokens of section 2 only (no raw colours in the diff);
- each state of the journey table (loading, empty, error, no permission, offline);
- the copy of `copy.md` word for word;
- no axe violations, and the keyboard path of the journey works;
- the agent mark wherever an agent did something.
