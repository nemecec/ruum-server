# Brand directions

Status: `decided`: the person chose **Graphite** (decision 0005 B-5); it is applied as the first
theme package, `web/src/design/themes/graphite/` (design 12.6). Proposal of 2026-11-09, written by
the visual designer agent (design 18.4, 12.5). The brand
direction is a decision of the person: this page and `directions.html` prepare three directions to
choose from. Nothing here changes the product's tokens or screens.

| File | What it holds |
|---|---|
| [directions.html](directions.html) | The three directions: character, palette (light and dark), type pairing, icon style, wordmark and app icon sketch, and the same three mock screens (file browser with an agent's change, share dialog, agents page) in light and dark. Open it in a browser. It is self-contained: inline CSS and SVG, no scripts, Google Fonts only for the type specimens. |
| [palettes.mjs](palettes.mjs) | The colour sets of the three directions, under the token names of `web/src/design/tokens/color.<theme>.tokens.json`, and the contrast pairs. |
| [build.mjs](build.mjs) | Draws `directions.html` from `palettes.mjs` and checks every pair first. Run `node docs/ux/brand/build.mjs` (exit 1 when a pair fails); `--md` prints the colour tables below. |

## The three directions

| | 1. Paper and spruce | 2. Marker | 3. Graphite |
|---|---|---|---|
| Character | The quiet notebook: warm paper, deep spruce, a soft serif. Calm first; the jokes live in the words. | The highlighter: ink, off-white and a yellow highlighter. The playful voice, made visible. | The precise instrument: cool graphite, fjord blue, square corners and mono. Exact and quiet. |
| Primary | Spruce `#1d5c57` / `#72c2b9` | Ink `#1c1b17` / yellow `#ffd84d` | Fjord `#0b5c86` / `#6cb8e6` |
| Agent colour | Clay `#8a4718` / `#e9a672` (as today) | Cobalt `#2a4bc4` / `#9db0ff` | Raspberry plum `#8f3f71` / `#e79ac7` |
| Titles | Fraunces 600 | Bricolage Grotesque 700 | IBM Plex Sans 600 |
| Body | System UI (as today) | Figtree | IBM Plex Sans |
| Mono | System mono (as today) | System mono (as today) | IBM Plex Mono, also for sizes and times |
| Corners (sm, md, lg) | 4, 8, 12 px (as today) | 6, 12, 18 px | 2, 3, 4 px |
| Icons | 1.5 px outline, round caps and joins (as today) | 2 px outline, round; a yellow fill behind the current page's icon | 1.5 px outline, square caps, mitred joins |
| Agent mark | Solid rounded square with a key cut (as today) | Solid circle with a key cut | Solid square with a key cut |
| Wordmark | Lowercase "ruum" as one round, even line | "Ruum" in heavy round strokes with a yellow swipe behind "uu" | Rectilinear "ruum" in four corner brackets |
| Colour tokens that change | none | 23 of 25 light, 25 of 25 dark | 22 of 25 light, 25 of 25 dark |

All three keep the rules of the design system: colour never carries meaning alone, the agent
colour belongs only to agents, sentence case, no emoji, no purple-to-blue gradients, no sparkle
for agents. The fonts all cover Estonian, German, Finnish and Polish letters (latin-ext).

## Contrast

`build.mjs` checks every pair of `docs/ux/tools/contrast.mjs` (WCAG 2.2: 4.5:1 for text, 3:1 for
control borders, the focus ring, icons and the primary edge), plus "text-muted on selected" and
"agent on selected", in both themes of all three directions, and the 8 tag colours of today on
each new surface (3:1). All 252 pairs pass. The lowest text pairs:

| Direction | Light | Dark |
|---|---|---|
| Paper and spruce | text-faint on canvas, 4.89:1 | text-faint on surface, 5.17:1 |
| Marker | text-faint on canvas, 5.28:1 | text-faint on surface, 5.29:1 |
| Graphite | text-faint on canvas, 5.02:1 | text-faint on surface, 5.50:1 |

After a direction is applied, `node docs/ux/tools/contrast.mjs` checks the real token files again.

## How a chosen direction maps onto the token files

The look lives in `web/src/design/tokens/` (design 12.5): `base.tokens.json` (type, spacing,
radius, sizes, motion) and `color.light.tokens.json` / `color.dark.tokens.json` (the colours and
shadows, with the same names in both files). Components use only tokens. So a direction is a set
of new token values; screens and components do not change.

### One-time preparation (the same for every direction)

Four things that a direction needs are not tokens today. Each is a small, additive change, made
once by a UI card, with values equal to today's, so nothing looks different until a direction is
applied. After this, applying a direction (or switching later) touches only tokens and the two
brand assets.

1. **A title font.** New tokens `font.family.display` (value today: the same as
   `font.family.sans`) and `font.weight.display` (today 600, as `font.weight.strong`). The
   composite tokens `text.lg`, `text.xl` and `text.2xl` (h2 and h1) refer to them instead of
   `{font.family.sans}` and `{font.weight.strong}`.
2. **Icon stroke.** New base tokens `icon.stroke-width` (1.5), `icon.linecap` (round) and
   `icon.linejoin` (round). `web/src/ui/icon/Icon.tsx` sets the stroke from these custom
   properties instead of the literal `1.5` (it also has 1.2 and 1.4 in the file type icons;
   those become the same token).
3. **Control edge.** New tokens `border.width.control` (1 px, the edge of buttons, inputs and
   selects) and `shadow.control` (none today; the hard edge of the primary button in Marker).
4. **Numbers.** Optional, Graphite only: new token `font.family.numeric` (today the sans stack),
   used by the size and time cells of lists.

Two brand assets are not tokens and change with any direction:

- **Wordmark.** `Brand` in `web/src/ui/shell/Shell.tsx` shows the text "Ruum" in
  `--font-weight-brand`. It becomes an inline SVG component drawn from the sketch in
  `directions.html`, coloured only with `--color-*` custom properties, so it follows light and
  dark. Its accessible name stays "Ruum".
- **App icon.** `web/public/favicon.svg` is still the default icon of the build tool (purple
  lightning). It must be replaced whatever the choice; the sketches are in `directions.html`.

**Fonts are self-hosted.** Design-system 2.2 says nothing is sent to a font host (this matters to
EU customers). A direction with a web font adds its WOFF2 files under `web/public/fonts/` and one
`@font-face` file; the font stacks keep the system fonts as fallback. All the fonts named here are
under the SIL Open Font License. Google Fonts is used only by the preview page.

### 1. Paper and spruce

| File | Tokens | New value |
|---|---|---|
| `color.light.tokens.json`, `color.dark.tokens.json` | none | (the values of today) |
| `base.tokens.json` | `font.family.display` | `"Fraunces", "Iowan Old Style", Georgia, serif` |
| | `font.weight.display` | stays 600 (Fraunces 600; the browser picks the optical size) |
| Assets | wordmark, app icon | the round monoline "ruum"; the spruce square with a white "r" |

### 2. Marker

| File | Tokens | New value |
|---|---|---|
| `color.light.tokens.json`, `color.dark.tokens.json` | every colour except the tags | the Marker table below |
| | `shadow-2` colour and `shadow.2` | light: `#1c1b17`, alpha 1, offset 5 / 5 px, blur 0 (a hard ink edge on dialogs and menus); dark: `#000000`, the same offsets |
| | `shadow.control` | light: `#1c1b17`, offset 3 / 3 px, blur 0; dark: `#000000` |
| `base.tokens.json` | `font.family.display` | `"Bricolage Grotesque", system-ui, ...` (the system stack) |
| | `font.family.sans` | `"Figtree", system-ui, ...` |
| | `font.weight.display` | 700 (titles); `font.weight.strong` stays 600 for labels |
| | `radius.sm`, `radius.md`, `radius.lg` | 6, 12, 18 px |
| | `border.width.control` | 2 px |
| | `icon.stroke-width` | 2 |
| Assets | wordmark, app icon | heavy "Ruum" with the yellow swipe; the yellow square with the ink "R" |

Marker keeps its yellow off calm screens by token use, not by new rules: errors, security and
billing use `danger`, `warning` and plain `text`, which are not yellow. The design-system
identity text (section 1, "a clay-ochre for agents") changes to cobalt.

### 3. Graphite

| File | Tokens | New value |
|---|---|---|
| `color.light.tokens.json`, `color.dark.tokens.json` | every colour except the tags | the Graphite table below |
| | `shadow-1`, `shadow-2` colour | light: `#16191d` (alpha 0.08 and 0.14 as today); dark as today |
| `base.tokens.json` | `font.family.sans`, `font.family.display` | `"IBM Plex Sans", system-ui, ...` |
| | `font.family.mono`, `font.family.numeric` | `"IBM Plex Mono", ui-monospace, ...` |
| | `radius.sm`, `radius.md`, `radius.lg` | 2, 3, 4 px |
| | `icon.linecap`, `icon.linejoin` | square, miter |
| Assets | wordmark, app icon | the rectilinear "ruum" in corner brackets; the outlined square with "ru" |

The design-system identity text (section 1, "warm neutrals", "spruce", "clay-ochre for agents")
changes to cool graphite, fjord blue and plum.

### Colour tables

The tag colours (spruce, moss, clay, brick, ochre, slate, plum, sand) stay as they are in every
direction; they pass 3:1 on each new surface.

Paper and spruce (the values of today):

| Token | Light | Dark |
|---|---|---|
| `canvas` | `#faf8f4` | `#161513` |
| `surface` | `#ffffff` | `#1e1c19` |
| `sunken` | `#f2efe9` | `#121110` |
| `border` | `#e0dbd2` | `#37332d` |
| `border-control` | `#8a8377` | `#7d766a` |
| `text` | `#24211c` | `#ede9e2` |
| `text-muted` | `#5c564c` | `#aba497` |
| `text-faint` | `#736c61` | `#948d81` |
| `primary` | `#1d5c57` | `#72c2b9` |
| `primary-hover` | `#164844` | `#8fd1c9` |
| `on-primary` | `#ffffff` | `#0e1d1c` |
| `primary-soft` | `#e2eeeb` | `#1e3431` |
| `link` | `#1d5c57` | `#82cbc2` |
| `focus` | `#1d5c57` | `#72c2b9` |
| `selected` | `#e2eeeb` | `#1e3431` |
| `danger` | `#a3301c` | `#f08b74` |
| `danger-hover` | `#86271a` | `#f4a693` |
| `on-danger` | `#ffffff` | `#1a0c08` |
| `danger-soft` | `#f7e6e1` | `#3b201a` |
| `success` | `#2b6439` | `#82c291` |
| `success-soft` | `#e4efe5` | `#1d2e21` |
| `warning` | `#7f5300` | `#e2b455` |
| `warning-soft` | `#faefd8` | `#33280f` |
| `agent` | `#8a4718` | `#e9a672` |
| `agent-soft` | `#f5e7da` | `#3a2a1d` |

Marker:

| Token | Light | Dark |
|---|---|---|
| `canvas` | `#fbf9f1` | `#151512` |
| `surface` | `#ffffff` | `#1f1e1a` |
| `sunken` | `#f3efe0` | `#0f0f0d` |
| `border` | `#e3ddc8` | `#36342d` |
| `border-control` | `#84806e` | `#7e7a6c` |
| `text` | `#1c1b17` | `#f3f0e4` |
| `text-muted` | `#55524a` | `#b1ac9c` |
| `text-faint` | `#6c685d` | `#969181` |
| `primary` | `#1c1b17` | `#ffd84d` |
| `primary-hover` | `#3a3830` | `#ffe27a` |
| `on-primary` | `#fff6c8` | `#1c1b17` |
| `primary-soft` | `#fff1a8` | `#3a3418` |
| `link` | `#1c1b17` | `#ffe27a` |
| `focus` | `#2a4bc4` | `#9db0ff` |
| `selected` | `#fff4bf` | `#34301a` |
| `danger` | `#b42318` | `#ff8a7a` |
| `danger-hover` | `#931c13` | `#ffa395` |
| `on-danger` | `#ffffff` | `#1f0b08` |
| `danger-soft` | `#fde7e4` | `#3d1e1a` |
| `success` | `#1f6b3a` | `#7fd19a` |
| `success-soft` | `#e3f2e6` | `#1b2e21` |
| `warning` | `#7a5200` | `#f0c060` |
| `warning-soft` | `#fdf0cf` | `#352a10` |
| `agent` | `#2a4bc4` | `#9db0ff` |
| `agent-soft` | `#e6ebfb` | `#22284a` |

In Marker light, `link` is ink like the text: links must keep their underline everywhere, not
only in running text (a component rule that already holds for running text).

Graphite:

| Token | Light | Dark |
|---|---|---|
| `canvas` | `#f6f7f8` | `#0f1215` |
| `surface` | `#ffffff` | `#171b20` |
| `sunken` | `#eaecef` | `#1d2228` |
| `border` | `#d9dde2` | `#2a3038` |
| `border-control` | `#7b828c` | `#6f7883` |
| `text` | `#16191d` | `#e6e9ed` |
| `text-muted` | `#4b525b` | `#a3abb5` |
| `text-faint` | `#646b75` | `#8a929c` |
| `primary` | `#0b5c86` | `#6cb8e6` |
| `primary-hover` | `#084a6c` | `#8fcbef` |
| `on-primary` | `#ffffff` | `#06202e` |
| `primary-soft` | `#e1eef6` | `#13293a` |
| `link` | `#0b5c86` | `#7cc2ec` |
| `focus` | `#0b5c86` | `#6cb8e6` |
| `selected` | `#e1eef6` | `#152a3a` |
| `danger` | `#b3261e` | `#ff8f85` |
| `danger-hover` | `#921f18` | `#ffa9a1` |
| `on-danger` | `#ffffff` | `#2a0b08` |
| `danger-soft` | `#fbe6e4` | `#3a1a18` |
| `success` | `#1d6b45` | `#6fcf97` |
| `success-soft` | `#e2f1e8` | `#142b1e` |
| `warning` | `#7d5400` | `#e8b860` |
| `warning-soft` | `#fbf0d6` | `#30260f` |
| `agent` | `#8f3f71` | `#e79ac7` |
| `agent-soft` | `#f6e6f0` | `#36202e` |

### Steps to apply a chosen direction (a UI card)

1. Do the one-time preparation above, if it is not done yet (no visual change).
2. Copy the direction's values into `color.light.tokens.json`, `color.dark.tokens.json` and
   `base.tokens.json` (hex and the `components` array; `palettes.mjs` has the hex values).
3. Add the WOFF2 files and the `@font-face` file, if the direction has web fonts.
4. Replace the wordmark in `Brand` and `web/public/favicon.svg`.
5. `npm run tokens`, `node docs/ux/tools/contrast.mjs`, and update the visual baselines of the
   catalogue and the e2e screenshots (every screenshot changes; that is expected).
6. Update the tables of `docs/ux/design-system.md` (2.1, 2.2, 2.4, 2.6) and its identity text.

## How to choose

Ask three questions, in this order.

1. **Who should feel at home first?** Small studios and offices that keep their work in Ruum
   (Paper and spruce, Marker), or developers and IT people who connect agents to it (Graphite).
2. **Where should the playfulness live?** Only in the words (Paper and spruce, Graphite), or in the
   words and the pictures (Marker).
3. **How much change now?** Paper and spruce changes no colour and adds one self-hosted font
   (Fraunces, for titles). Marker and Graphite change every colour token and add two self-hosted
   font families.

**My recommendation: 1, Paper and spruce.**

Ruum's promise is "a friendly colleague who also keeps your work safe". The voice already carries
the friendly part, and it does it well. The visuals should carry the safe part: warm, quiet, and
the same on every screen, so that a security or delete dialog never looks like a joke. Paper and
spruce does that. It keeps the clay agent colour that people already learn as "an agent did
this", it passes every contrast pair today, and it needs no colour change, so the risk and the
cost are the lowest. The soft serif in titles and the round wordmark add the character that the
plain system font lacks, and they replace the placeholder favicon.

Choose **Marker** if you want Ruum to be recognisable from across the room and the playful voice
to show before anyone reads a word. It costs more care on calm screens: the yellow and the hard
edges must stay off errors, security and billing. Choose **Graphite** if the first buyers are
developers and IT, and you accept a look that is cooler than the voice.
