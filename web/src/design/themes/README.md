# Theme packages

A theme package is the look of Ruum as data (design 12.6): token values and image and font
files, never code. The default brand, **Graphite** (decision 0005 B-5), is the first package
(`graphite/`). The token build applies the default theme over the core tokens, so screens and
components change only through tokens.

## The format (format 1)

```
<id>/
  theme.json                    the manifest (schema: theme.schema.json)
  tokens/base.tokens.json       optional: new values for tokens of ../tokens/base.tokens.json
  tokens/color.light.tokens.json optional: new values for tokens of ../tokens/color.light.tokens.json
  tokens/color.dark.tokens.json  optional: new values for tokens of ../tokens/color.dark.tokens.json
  assets/wordmark.svg           optional: the wordmark of the top bar
  assets/favicon.svg            the browser tab icon
  assets/*.png                  app icons: 180 (apple-touch), 192 and 512 px
  fonts/*.woff2, fonts/OFL.txt  optional: self-hosted fonts and their licence
```

- **Manifest**: `format` (1), `id`, `name`, `version`, `author`, `license`, the token files, the
  assets, the fonts (family, weight, style, subset `latin` or `latin-ext`, file) and the font
  licence. Every file of the folder must be named by the manifest.
- **Token overrides** use the W3C Design Tokens format of the core files. A theme gives new
  values to tokens that the core defines; it cannot add a token, change a type or add
  `$extensions`. Values must be of the token's type: an sRGB colour whose components match its
  hex, a dimension in px or rem, a list of font family names, a weight, a number, a keyword from
  the list the core gives (`icon.linecap`: round, square, butt; `icon.linejoin`: round, miter,
  bevel), or a reference `{a.b}` to a token of the same type.
- **SVGs** hold only plain shapes (`svg`, `g`, `path`, `rect`, `circle`, `ellipse`, `line`,
  `polyline`, `polygon`, `title`) with geometry and paint attributes. Paint is `none`,
  `currentColor`, a hex colour or `var(--color-<token>, #fallback)`. No script, style, text,
  links, `use`, filters or event attributes. The wordmark uses `currentColor` (the text colour of
  the top bar) and `var(--color-*)`, so it follows light and dark; draw the name itself in
  `currentColor`, because forced colours repaint only that. The favicon uses fixed colours (an
  image cannot read the page's properties).
- **Fonts** are WOFF2 files in the package. The build writes one `@font-face` per file with the
  unicode range of its subset. Nothing loads from a font host.
- **Limits**: 64 files, 4 MB in all; theme.json 64 KB, a token file 256 KB, an SVG 64 KB, a PNG
  512 KB, a WOFF2 file 256 KB, a text file 128 KB. Only `.json`, `.svg`, `.png`, `.woff2`, `.txt`
  and `.md` files; no links.

## The checks

`node scripts/check-theme.mjs [folder ...]` (in `npm run lint`) checks the manifest against the
schema, the files (types, sizes, PNG and WOFF2 signatures, PNG sizes), the token overrides, the
SVGs, and the WCAG 2.2 contrast of every pair of `scripts/theme-lib.mjs` in light and dark, with
the theme over the core: 35 text and control pairs and the 8 tag colours, per scheme. A failing
theme is refused, and the failing pairs are listed with their ratios. `--verbose` lists every pair.

## The build

`npm run tokens` (`scripts/build-tokens.mjs`) checks the default theme (`DEFAULT_THEME` in
`scripts/theme-lib.mjs`), merges it over the core tokens and writes:

- `src/design/generated/tokens.css`: the `@font-face` rules and the custom properties;
- `src/design/generated/tokens.ts`: the values for code and the theme's id, name and version;
- `src/design/generated/brand.ts`: the wordmark as a tree of shapes, which `Brand` draws inline;
- `public/favicon.svg`, `public/apple-touch-icon.png`, `public/icon-192.png`,
  `public/icon-512.png` and `public/manifest.webmanifest`.

`npm run tokens -- --check` (in lint) fails when a committed output is out of date.

## Not yet

- Loading a theme at runtime (the Appearance page of the admin console, a space's logo and
  accent within limits, upload of a theme file) and signing themes through the registry: the
  checks here are the ones installation will run.
- A copy pack (product name, tone tweaks) and empty-state illustrations: not in format 1.

## Graphite

Cool graphite neutrals, fjord blue for actions, raspberry plum for agents, IBM Plex Sans for text
and titles, IBM Plex Mono for code, sizes and times, corners of 2, 3 and 4 px, and icon strokes
with square caps and mitred joins. The wordmark is a rectilinear "ruum" in four corner brackets;
the app icon is "ru" in a fjord frame. The source of the palette and the directions is
`docs/ux/brand/`. The PNG app icons are drawn from the favicon geometry (axis-aligned strokes,
scaled to 78% so the iOS corner rounding does not touch the frame).

Lowest text pairs: light text-faint on sunken 4.55:1, dark text-faint on sunken 5.09:1.

After the UX panel (V-1, V-2, access 1): the plum tag is a blue violet (`#5b4a94`, dark
`#b0a4e0`), so a tag no longer looks like the agent colour; dark `sunken` is a raised tone
(`#1d2228`) between surface and border, so table headers, the top bar and the rail read as bands,
not holes, and light `sunken` steps a little further from the canvas (`#eaecef`); the letters of
the wordmark use `currentColor` (the colour of `.ui-brand`, the text colour), so forced colours
repaint them.
