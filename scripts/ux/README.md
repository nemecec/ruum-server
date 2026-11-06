# Browser driver for agents (`drive.mjs`)

`drive.mjs` lets an agent use the web UI like a person, from the shell. It keeps one Chromium
open in the background, and each command acts on it: open a page, click, type, upload, read the
page, take a screenshot. Use it for UX checks, for reproducing a bug report, and for designs that
need to be tried on a phone width or in dark mode (docs/DESIGN.md, section 18.4.1 and 18.9).

It uses Playwright from `web/node_modules`. Install it once:

```sh
cd web && npm ci && npx playwright install chromium
```

## A session

```sh
node scripts/ux/drive.mjs start --port 9333 --viewport 1280x800 --scheme light
node scripts/ux/drive.mjs goto http://localhost:5182/
node scripts/ux/drive.mjs snapshot
node scripts/ux/drive.mjs click "link:Sign in with email"
node scripts/ux/drive.mjs fill "Login Name" ann@example.test
node scripts/ux/drive.mjs press Enter
node scripts/ux/drive.mjs screenshot /tmp/files.png
node scripts/ux/drive.mjs console
node scripts/ux/drive.mjs stop
```

- A session is one port. `--port` (default 9333, or `RUUM_DRIVE_PORT`) goes on every command,
  before or after the command name. Two sessions on two ports have two browsers, with separate
  cookies and storage, so two agents (or two users) can work side by side.
- `start` makes a new, empty profile each time. To keep a sign-in for the next session, start it
  again with `--profile <folder>`; `status` prints the profile folder of the session.
- `--viewport 390x844` is a phone (touch, 2x pixels); `--scheme dark` sets
  `prefers-color-scheme: dark`. `--headed` shows the window.
- The profiles, downloads and the log are in `$TMPDIR/ruum-drive/<port>/` (or `RUUM_DRIVE_DIR`).
  `stop` does not delete them; the system cleans the temporary folder.

## Commands

| Command | What it does |
|---|---|
| `goto <url>` | Opens the URL. A relative URL uses the origin of the current page. |
| `click <target>` | Clicks the element. |
| `fill <label> <text>` | Types into a field, found by its label, placeholder or name. |
| `select <label> <option>` | Picks an option of a select by its text. |
| `press <key>` | Presses a key: `Enter`, `Escape`, `Tab`, `Control+A`, ... |
| `upload <label> <file...>` | Gives files to a file input (also a hidden one), or to the file chooser that a button opens. |
| `wait <text>` or `wait <ms>` | Waits until the text is visible (15 s, `--timeout`), or for some milliseconds. |
| `back` | Goes back in the history. |
| `snapshot [--full]` | Prints the accessibility tree with refs (`[ref=e12]`); the first 150 lines unless `--full`. |
| `text` | Prints the visible text of the page (the first 4000 characters). |
| `screenshot <file> [--full-page]` | Saves a PNG of the viewport, or of the whole page. |
| `console [--all]` | Prints console errors, page errors, failed requests and `5xx` answers since the last call. `--all` adds warnings and downloads. |
| `tabs`, `tab <n>` | Lists the tabs, or makes one the current tab. A new tab (a preview) becomes the current tab. |
| `status` | The settings, the profile folder and the current page. |

A target is one of:

- a ref from the last `snapshot`: `e12` (refs change after a page load; take a new snapshot);
- `role:name`, for example `button:Upload`, `link:Trash`, `textbox:Search by name`; an exact
  name first, then a part of the name;
- the visible text of the element.

When a target matches more than one visible element, the command fails and lists the matches.
Add `--nth N` (0-based) or use a ref. The driver waits up to 5 seconds for a target to appear.

Downloads go to `$TMPDIR/ruum-drive/<port>/downloads/`; `console --all` prints each saved file.
Every command prints a short result and exits with 1 on an error, so scripts can stop on it.

## Signing in

The web UI signs in through Zitadel. With the laptop stack (`scripts/dev.sh up`), make a test user
with a password (the e2e specs do it with `web/e2e/support/zitadel.ts`), then:

```sh
node scripts/ux/drive.mjs goto http://localhost:5182/
node scripts/ux/drive.mjs click "link:Sign in with email"   # every button opens the Zitadel login for now
node scripts/ux/drive.mjs fill "Login Name" <email>
node scripts/ux/drive.mjs click button:Next
node scripts/ux/drive.mjs fill Password <password>
node scripts/ux/drive.mjs click button:Next
node scripts/ux/drive.mjs click button:Skip                     # the 2-factor prompt, once per user
```

Never type a real person's password into the driver. Use test users of the laptop stack only.
