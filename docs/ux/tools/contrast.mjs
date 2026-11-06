// Checks the colour pairs of docs/ux/design-system.md against WCAG 2.2 AA.
// Text pairs need 4.5:1; large text, icons, borders of controls and the focus ring need 3:1.
// Run: node contrast.mjs   (exit code 1 when a pair fails)

const themes = {
  light: {
    canvas: '#FAF8F4', surface: '#FFFFFF', sunken: '#F2EFE9',
    border: '#E0DBD2', 'border-control': '#8A8377',
    text: '#24211C', 'text-muted': '#5C564C', 'text-faint': '#736C61',
    primary: '#1D5C57', 'primary-hover': '#164844', 'on-primary': '#FFFFFF',
    'primary-soft': '#E2EEEB', link: '#1D5C57', focus: '#1D5C57', selected: '#E2EEEB',
    danger: '#A3301C', 'danger-soft': '#F7E6E1', 'on-danger': '#FFFFFF',
    success: '#2B6439', 'success-soft': '#E4EFE5',
    warning: '#7F5300', 'warning-soft': '#FAEFD8',
    agent: '#8A4718', 'agent-soft': '#F5E7DA',
  },
  dark: {
    canvas: '#161513', surface: '#1E1C19', sunken: '#121110',
    border: '#37332D', 'border-control': '#7D766A',
    text: '#EDE9E2', 'text-muted': '#ABA497', 'text-faint': '#948D81',
    primary: '#72C2B9', 'primary-hover': '#8FD1C9', 'on-primary': '#0E1D1C',
    'primary-soft': '#1E3431', link: '#82CBC2', focus: '#72C2B9', selected: '#1E3431',
    danger: '#F08B74', 'danger-soft': '#3B201A', 'on-danger': '#1A0C08',
    success: '#82C291', 'success-soft': '#1D2E21',
    warning: '#E2B455', 'warning-soft': '#33280F',
    agent: '#E9A672', 'agent-soft': '#3A2A1D',
  },
}

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
  ['link', 'canvas', 4.5, 'links'],
  ['link', 'surface', 4.5, 'links on cards'],
  ['link', 'selected', 4.5, 'links in a selected row'],
  ['on-primary', 'primary', 4.5, 'primary button label'],
  ['on-primary', 'primary-hover', 4.5, 'primary button label, hover'],
  ['primary', 'primary-soft', 4.5, 'text on primary-soft (info note)'],
  ['on-danger', 'danger', 4.5, 'danger button label'],
  ['danger', 'surface', 4.5, 'error text'],
  ['danger', 'danger-soft', 4.5, 'error note'],
  ['success', 'success-soft', 4.5, 'success note'],
  ['success', 'surface', 4.5, 'success text'],
  ['warning', 'warning-soft', 4.5, 'warning note'],
  ['agent', 'agent-soft', 4.5, 'agent badge'],
  ['agent', 'surface', 4.5, 'agent name in text'],
  ['border-control', 'surface', 3, 'input and checkbox border'],
  ['border-control', 'canvas', 3, 'input border on canvas'],
  ['focus', 'canvas', 3, 'focus ring'],
  ['focus', 'surface', 3, 'focus ring on cards'],
  ['primary', 'canvas', 3, 'primary button edge, icons'],
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
