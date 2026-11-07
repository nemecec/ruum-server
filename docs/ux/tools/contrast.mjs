// Checks the colour pairs of the design tokens (docs/ux/design-system.md) against WCAG 2.2 AA.
// Text pairs need 4.5:1; large text, icons, borders of controls and the focus ring need 3:1.
// Run: node contrast.mjs   (exit code 1 when a pair fails)

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

// The colours come from the token files of the web UI (W3C Design Tokens format), the single
// source of the design system (design 12.5): web/src/design/tokens/color.<theme>.tokens.json.
const tokens = path.join(path.dirname(fileURLToPath(import.meta.url)), '../../../web/src/design/tokens')

/** The flat colour map of one theme file: `text-muted`, `tag-spruce`, ... -> `#rrggbb`. */
function load(theme) {
  const json = JSON.parse(fs.readFileSync(path.join(tokens, `color.${theme}.tokens.json`), 'utf8'))
  const out = {}
  const walk = (node, prefix) => {
    for (const [k, v] of Object.entries(node)) {
      if (k.startsWith('$') || typeof v !== 'object') continue
      if ('$value' in v) {
        if (v.$value.hex && v.$value.alpha === undefined) out[prefix + k] = v.$value.hex
      } else walk(v, `${prefix}${k}-`)
    }
  }
  walk(json.color, '')
  return out
}

const themes = { light: load('light'), dark: load('dark') }

// [foreground, background, minimum ratio, what]
const pairs = [
  ['text', 'canvas', 4.5, 'body text'],
  ['text', 'surface', 4.5, 'body text on cards'],
  ['text', 'sunken', 4.5, 'text on sunken areas'],
  ['text', 'selected', 4.5, 'text in a selected row'],
  ['text-muted', 'canvas', 4.5, 'secondary text'],
  ['text-muted', 'surface', 4.5, 'secondary text on cards'],
  ['text-muted', 'sunken', 4.5, 'secondary text on sunken areas'],
  ['text-faint', 'surface', 4.5, 'placeholders and timestamps'],
  // text-faint is only for surface, canvas and sunken: on selected and primary-soft it is under
  // 4.5:1 (UX panel A10), so Text tone="faint" is not used there.
  ['text-faint', 'canvas', 4.5, 'timestamps on the page'],
  ['link', 'canvas', 4.5, 'links'],
  ['link', 'surface', 4.5, 'links on cards'],
  ['link', 'selected', 4.5, 'links in a selected row'],
  ['on-primary', 'primary', 4.5, 'primary button label'],
  ['on-primary', 'primary-hover', 4.5, 'primary button label, hover'],
  ['primary', 'primary-soft', 4.5, 'text on primary-soft (info note)'],
  ['on-danger', 'danger', 4.5, 'danger button label'],
  ['on-danger', 'danger-hover', 4.5, 'danger button label, hover'],
  ['danger', 'surface', 4.5, 'error text'],
  ['danger', 'danger-soft', 4.5, 'error note'],
  ['success', 'success-soft', 4.5, 'success note'],
  ['success', 'surface', 4.5, 'success text'],
  ['warning', 'warning-soft', 4.5, 'warning note'],
  ['agent', 'agent-soft', 4.5, 'agent badge'],
  ['agent', 'surface', 4.5, 'agent name in text'],
  ['border-control', 'surface', 3, 'input and checkbox border'],
  ['border-control', 'canvas', 3, 'input border on canvas'],
  ['border-control', 'sunken', 3, 'search field border in the top bar'],
  ['focus', 'canvas', 3, 'focus ring'],
  ['focus', 'surface', 3, 'focus ring on cards'],
  ['primary', 'canvas', 3, 'primary button edge, icons'],
  ['primary', 'selected', 3, 'selected nav edge and text'],
  ['text-faint', 'canvas', 3, 'empty state illustration'],
  ...['spruce', 'moss', 'clay', 'brick', 'ochre', 'slate', 'plum', 'sand'].map((t) => [`tag-${t}`, 'surface', 3, `tag colour ${t} (edge and dot)`]),
]

function lum(hex) {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
  const l = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
}
function ratio(a, b) {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p)
  return (x + 0.05) / (y + 0.05)
}

let failed = 0
for (const [name, t] of Object.entries(themes)) {
  console.log(`\n${name}`)
  for (const [fg, bg, min, what] of pairs) {
    const r = ratio(t[fg], t[bg])
    const ok = r >= min
    if (!ok) failed++
    console.log(`${ok ? 'pass' : 'FAIL'}  ${r.toFixed(2).padStart(5)} >= ${min}  ${fg} on ${bg} (${what})`)
  }
}
console.log(failed ? `\n${failed} pairs fail` : '\nall pairs pass')
process.exit(failed ? 1 : 0)
