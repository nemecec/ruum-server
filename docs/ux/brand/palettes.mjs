// The colour sets of the three brand directions (docs/ux/brand/README.md).
// The names are the names of web/src/design/tokens/color.<theme>.tokens.json, so a chosen set
// can replace the values in those files one to one. build.mjs draws directions.html from these
// values and checks every pair with the WCAG formula of docs/ux/tools/contrast.mjs.

export const directions = {
  paper: {
    light: {
      canvas: '#faf8f4', surface: '#ffffff', sunken: '#f2efe9', border: '#e0dbd2', 'border-control': '#8a8377',
      text: '#24211c', 'text-muted': '#5c564c', 'text-faint': '#736c61',
      primary: '#1d5c57', 'primary-hover': '#164844', 'on-primary': '#ffffff', 'primary-soft': '#e2eeeb',
      link: '#1d5c57', focus: '#1d5c57', selected: '#e2eeeb',
      danger: '#a3301c', 'danger-hover': '#86271a', 'on-danger': '#ffffff', 'danger-soft': '#f7e6e1',
      success: '#2b6439', 'success-soft': '#e4efe5', warning: '#7f5300', 'warning-soft': '#faefd8',
      agent: '#8a4718', 'agent-soft': '#f5e7da',
    },
    dark: {
      canvas: '#161513', surface: '#1e1c19', sunken: '#121110', border: '#37332d', 'border-control': '#7d766a',
      text: '#ede9e2', 'text-muted': '#aba497', 'text-faint': '#948d81',
      primary: '#72c2b9', 'primary-hover': '#8fd1c9', 'on-primary': '#0e1d1c', 'primary-soft': '#1e3431',
      link: '#82cbc2', focus: '#72c2b9', selected: '#1e3431',
      danger: '#f08b74', 'danger-hover': '#f4a693', 'on-danger': '#1a0c08', 'danger-soft': '#3b201a',
      success: '#82c291', 'success-soft': '#1d2e21', warning: '#e2b455', 'warning-soft': '#33280f',
      agent: '#e9a672', 'agent-soft': '#3a2a1d',
    },
  },
  marker: {
    light: {
      canvas: '#fbf9f1', surface: '#ffffff', sunken: '#f3efe0', border: '#e3ddc8', 'border-control': '#84806e',
      text: '#1c1b17', 'text-muted': '#55524a', 'text-faint': '#6c685d',
      primary: '#1c1b17', 'primary-hover': '#3a3830', 'on-primary': '#fff6c8', 'primary-soft': '#fff1a8',
      link: '#1c1b17', focus: '#2a4bc4', selected: '#fff4bf',
      danger: '#b42318', 'danger-hover': '#931c13', 'on-danger': '#ffffff', 'danger-soft': '#fde7e4',
      success: '#1f6b3a', 'success-soft': '#e3f2e6', warning: '#7a5200', 'warning-soft': '#fdf0cf',
      agent: '#2a4bc4', 'agent-soft': '#e6ebfb',
    },
    dark: {
      canvas: '#151512', surface: '#1f1e1a', sunken: '#0f0f0d', border: '#36342d', 'border-control': '#7e7a6c',
      text: '#f3f0e4', 'text-muted': '#b1ac9c', 'text-faint': '#969181',
      primary: '#ffd84d', 'primary-hover': '#ffe27a', 'on-primary': '#1c1b17', 'primary-soft': '#3a3418',
      link: '#ffe27a', focus: '#9db0ff', selected: '#34301a',
      danger: '#ff8a7a', 'danger-hover': '#ffa395', 'on-danger': '#1f0b08', 'danger-soft': '#3d1e1a',
      success: '#7fd19a', 'success-soft': '#1b2e21', warning: '#f0c060', 'warning-soft': '#352a10',
      agent: '#9db0ff', 'agent-soft': '#22284a',
    },
  },
  graphite: {
    light: {
      canvas: '#f6f7f8', surface: '#ffffff', sunken: '#eaecef', border: '#d9dde2', 'border-control': '#7b828c',
      text: '#16191d', 'text-muted': '#4b525b', 'text-faint': '#646b75',
      primary: '#0b5c86', 'primary-hover': '#084a6c', 'on-primary': '#ffffff', 'primary-soft': '#e1eef6',
      link: '#0b5c86', focus: '#0b5c86', selected: '#e1eef6',
      danger: '#b3261e', 'danger-hover': '#921f18', 'on-danger': '#ffffff', 'danger-soft': '#fbe6e4',
      success: '#1d6b45', 'success-soft': '#e2f1e8', warning: '#7d5400', 'warning-soft': '#fbf0d6',
      agent: '#8f3f71', 'agent-soft': '#f6e6f0',
    },
    dark: {
      canvas: '#0f1215', surface: '#171b20', sunken: '#1d2228', border: '#2a3038', 'border-control': '#6f7883',
      text: '#e6e9ed', 'text-muted': '#a3abb5', 'text-faint': '#8a929c',
      primary: '#6cb8e6', 'primary-hover': '#8fcbef', 'on-primary': '#06202e', 'primary-soft': '#13293a',
      link: '#7cc2ec', focus: '#6cb8e6', selected: '#152a3a',
      danger: '#ff8f85', 'danger-hover': '#ffa9a1', 'on-danger': '#2a0b08', 'danger-soft': '#3a1a18',
      success: '#6fcf97', 'success-soft': '#142b1e', warning: '#e8b860', 'warning-soft': '#30260f',
      agent: '#e79ac7', 'agent-soft': '#36202e',
    },
  },
}

// The tag colours stay as they are in every direction; they must still pass 3:1 on the surface.
export const tags = {
  light: { spruce: '#1d5c57', moss: '#2b6439', clay: '#8a4718', brick: '#a3301c', ochre: '#7f5300', slate: '#4a5868', plum: '#6e3b5e', sand: '#8a7a55' },
  dark: { spruce: '#72c2b9', moss: '#82c291', clay: '#e9a672', brick: '#f08b74', ochre: '#e2b455', slate: '#9aa8b8', plum: '#c99bbc', sand: '#c8b78e' },
}

// [foreground, background, minimum ratio, what]: the pairs of docs/ux/tools/contrast.mjs.
export const pairs = [
  ['text', 'canvas', 4.5, 'body text'],
  ['text', 'surface', 4.5, 'body text on cards'],
  ['text', 'sunken', 4.5, 'text on sunken areas'],
  ['text', 'selected', 4.5, 'text in a selected row'],
  ['text-muted', 'canvas', 4.5, 'secondary text'],
  ['text-muted', 'surface', 4.5, 'secondary text on cards'],
  ['text-muted', 'sunken', 4.5, 'secondary text on sunken areas'],
  ['text-muted', 'selected', 4.5, 'secondary text in a selected row'],
  ['text-faint', 'surface', 4.5, 'placeholders and timestamps'],
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
  ['agent', 'selected', 4.5, 'agent name in a selected row'],
  ['border-control', 'surface', 3, 'input and checkbox border'],
  ['border-control', 'canvas', 3, 'input border on canvas'],
  ['border-control', 'sunken', 3, 'search field border in the top bar'],
  ['focus', 'canvas', 3, 'focus ring'],
  ['focus', 'surface', 3, 'focus ring on cards'],
  ['primary', 'canvas', 3, 'primary button edge, icons'],
  ['primary', 'selected', 3, 'selected nav edge and text'],
  ['text-faint', 'canvas', 3, 'empty state illustration'],
]

function lum(hex) {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
  const l = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
}
export function ratio(a, b) {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p)
  return (x + 0.05) / (y + 0.05)
}

/** Every pair of every direction and theme: [{dir, theme, fg, bg, min, what, r, ok}]. */
export function check() {
  const out = []
  for (const [dir, themes] of Object.entries(directions)) {
    for (const [theme, t] of Object.entries(themes)) {
      for (const [fg, bg, min, what] of pairs) {
        const r = ratio(t[fg], t[bg])
        out.push({ dir, theme, fg, bg, min, what, r, ok: r >= min })
      }
      for (const [name, hex] of Object.entries(tags[theme])) {
        const r = ratio(hex, t.surface)
        out.push({ dir, theme, fg: `tag-${name}`, bg: 'surface', min: 3, what: `tag colour ${name}`, r, ok: r >= 3 })
      }
    }
  }
  return out
}
