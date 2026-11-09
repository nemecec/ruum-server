// Checks the colour pairs of the design tokens (docs/ux/design-system.md) against WCAG 2.2 AA.
// Text pairs need 4.5:1; large text, icons, borders of controls and the focus ring need 3:1.
// Run: node contrast.mjs   (exit code 1 when a pair fails)

// The colours are the effective tokens of the web UI: the core token files
// (web/src/design/tokens/, W3C Design Tokens format, design 12.5) with the default theme package
// over them (web/src/design/themes/, design 12.6). The pairs and the formula live in
// web/scripts/theme-lib.mjs, which the theme validator (web/scripts/check-theme.mjs) uses too.
import { DEFAULT_THEME, colours, effectiveTokens, loadTheme, pairs, ratio, themeDir } from '../../../web/scripts/theme-lib.mjs'

const trees = effectiveTokens(loadTheme(themeDir(DEFAULT_THEME)))
const themes = { light: colours(trees.light), dark: colours(trees.dark) }
console.log(`theme ${DEFAULT_THEME}`)

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
