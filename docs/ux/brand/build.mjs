// Builds docs/ux/brand/directions.html: the three brand directions side by side, with the same
// three mock screens in light and dark. The colours come from palettes.mjs; every text pair is
// checked with the WCAG formula of docs/ux/tools/contrast.mjs before the page is written.
//
//   node docs/ux/brand/build.mjs          write directions.html (exit 1 when a pair fails)
//   node docs/ux/brand/build.mjs --md     also print the colour tables for README.md
//
// The page is self-contained: inline CSS and SVG, no scripts, Google Fonts for the specimens.

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { directions, check, ratio } from './palettes.mjs'

const here = path.dirname(fileURLToPath(import.meta.url))

// ---------------------------------------------------------------------------------------------
// The directions: words, type, icon style, shape
// ---------------------------------------------------------------------------------------------

const SYS = `system-ui, -apple-system, "Segoe UI", Roboto, "Noto Sans", sans-serif`
const MONO = `ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace`

const meta = {
  paper: {
    n: 1,
    name: 'Paper and spruce',
    nick: 'The quiet notebook',
    line: 'Warm paper, deep spruce, a soft serif. Calm first; the jokes live in the words.',
    character:
      'Ruum looks like a well-kept notebook on a wooden desk. Warm paper neutrals, a deep spruce ' +
      'green for the one thing to do next, and a clay colour that belongs only to agents. Titles ' +
      'are set in Fraunces, a soft serif with round, slightly wobbly ends, so every page has a human ' +
      'hand in it; the body text stays in the system font, so Ruum feels at home on every device ' +
      'and reads well in Estonian, German or Polish. Nothing shouts. The playful voice does the ' +
      'smiling, and the visuals keep the promise that your work is safe. This is the closest to the ' +
      'tokens Ruum has today: choosing it changes no colour at all.',
    type: {
      head: 'Fraunces', headNote: 'titles and the wordmark, weight 600, optical size on',
      body: 'System UI', bodyNote: 'body, labels and tables, as today (no download)',
      mono: 'System mono', monoNote: 'key prefixes and code',
    },
    icons:
      'Outline icons at 20 px with a 1.5 px stroke, round caps and joins, soft corners (the Lucide ' +
      'style of today). The agent mark is a solid rounded square with a key cut.',
    wordmark:
      'Lowercase "ruum" as one even, round line, like a word written with a good pen. The two u\'s ' +
      'stand close together: a room with two people in it.',
    css: {
      '--fh': `"Fraunces", "Iowan Old Style", Georgia, serif`, '--fb': SYS, '--fm': MONO, '--hw': 600,
      '--r-sm': '4px', '--r-md': '8px', '--r-lg': '12px', '--isw': 1.5, '--cap': 'round', '--join': 'round',
      '--btn-bw': '1px', '--btn-shadow': 'none', '--hl': 'transparent',
    },
    agentRx: 4,
  },
  marker: {
    n: 2,
    name: 'Marker',
    nick: 'The highlighter',
    line: 'Ink, off-white and a yellow highlighter. The playful voice, made visible.',
    character:
      'Ruum looks like the wall of a lively studio: black ink on off-white paper, and a yellow ' +
      'highlighter on the one thing that matters on this screen (the selected row, the main note, ' +
      'the wordmark). Titles are set in Bricolage Grotesque, a bold grotesque with a bit of ' +
      'mischief; the body is Figtree, round and open. Buttons are chunky, with a hard ink edge. ' +
      'Agents wear cobalt, a colour nothing else uses, so their work stands out at a glance. This ' +
      'direction carries the informal voice into the pictures. Where data, security or money are at ' +
      'stake, the yellow and the hard edges step back and plain ink remains.',
    type: {
      head: 'Bricolage Grotesque', headNote: 'titles and the wordmark, weight 700',
      body: 'Figtree', bodyNote: 'body, labels and tables, weights 400 to 600',
      mono: 'System mono', monoNote: 'key prefixes and code',
    },
    icons:
      'Outline icons with a 2 px stroke, round caps, open shapes; the icon of the current page gets ' +
      'a yellow fill behind its line. The agent mark is a solid circle with a key cut.',
    wordmark:
      '"Ruum" in heavy round strokes, with a quick yellow highlighter swipe behind the "uu". It is ' +
      'the only place where the brand is allowed to be a little loud.',
    css: {
      '--fh': `"Bricolage Grotesque", ${SYS}`, '--fb': `"Figtree", ${SYS}`, '--fm': MONO, '--hw': 700,
      '--r-sm': '6px', '--r-md': '12px', '--r-lg': '18px', '--isw': 2, '--cap': 'round', '--join': 'round',
      '--btn-bw': '2px', '--btn-shadow': '3px 3px 0 var(--text)', '--hl': 'var(--primary-soft)',
    },
    agentRx: 8,
  },
  graphite: {
    n: 3,
    name: 'Graphite',
    nick: 'The precise instrument',
    line: 'Cool graphite, fjord blue, square corners and mono. Exact and quiet.',
    character:
      'Ruum looks like a well-made instrument for people who care about the exact state of things. ' +
      'Cool graphite neutrals, a deep fjord blue for actions, square corners and thin rules. IBM ' +
      'Plex Sans carries the text and IBM Plex Mono shows sizes, times and key prefixes, so numbers ' +
      'line up and read like facts. Agents are marked in a raspberry plum that appears nowhere ' +
      'else. There is no decoration at all: the playfulness lives only in the copy. It is the ' +
      'calmest of the three and the most at home next to developer tools, and the furthest from the ' +
      'warm voice.',
    type: {
      head: 'IBM Plex Sans', headNote: 'titles, weight 600; the same family as the body',
      body: 'IBM Plex Sans', bodyNote: 'body, labels and tables, weights 400 and 500',
      mono: 'IBM Plex Mono', monoNote: 'sizes, times, key prefixes and code',
    },
    icons:
      'Outline icons with a 1.5 px stroke, square caps and mitred joins, drawn on a strict 24 px ' +
      'grid. The agent mark is a solid square with a key cut.',
    wordmark:
      'A rectilinear "ruum" built from straight strokes, held in four corner brackets: the outline ' +
      'of a room, or a selection on a screen.',
    css: {
      '--fh': `"IBM Plex Sans", ${SYS}`, '--fb': `"IBM Plex Sans", ${SYS}`, '--fm': `"IBM Plex Mono", ${MONO}`, '--hw': 600,
      '--r-sm': '2px', '--r-md': '3px', '--r-lg': '4px', '--isw': 1.5, '--cap': 'square', '--join': 'miter',
      '--btn-bw': '1px', '--btn-shadow': 'none', '--hl': 'transparent',
    },
    agentRx: 1,
  },
}

const ids = Object.keys(meta)

// ---------------------------------------------------------------------------------------------
// SVG: icons, agent marks, wordmarks, app icons
// ---------------------------------------------------------------------------------------------

const iconPaths = {
  folder: '<path d="M3 7.5A2 2 0 0 1 5 5.5h4l2 2h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  file: '<path d="M6 3h8l4 4v14H6z"/><path d="M14 3v4h4M9 12h6M9 15.5h6"/>',
  image: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><circle cx="9" cy="9.5" r="1.5"/><path d="M20.5 16l-4.5-4.5-9 8"/>',
  share: '<circle cx="9" cy="8" r="3.5"/><path d="M3 20a6 6 0 0 1 12 0M18.5 8v6M15.5 11h6"/>',
  search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
  upload: '<path d="M12 15.5V4M7.5 8.5L12 4l4.5 4.5M4 15.5V19a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-3.5"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  trash: '<path d="M4 7h16M9 7V4h6v3M6.5 7l1 13h9l1-13"/>',
  key: '<circle cx="8" cy="15" r="4"/><path d="M11 12l8-8M15.5 7.5l2.5 2.5M13.5 9.5l2 2"/>',
  more: '<circle cx="5.5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="18.5" cy="12" r="1"/>',
  close: '<path d="M6 6l12 12M18 6L6 18"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
  chevron: '<path d="M6.5 9.5L12 15l5.5-5.5"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
}

function sprite() {
  const syms = Object.entries(iconPaths).map(([k, p]) => `<symbol id="i-${k}" viewBox="0 0 24 24">${p}</symbol>`)
  for (const id of ids) {
    const rx = meta[id].agentRx
    // A solid shape with a key-shaped cut: a ring of a bow and a bit with one tooth.
    syms.push(
      `<symbol id="am-${id}" viewBox="0 0 16 16"><mask id="amm-${id}"><rect width="16" height="16" fill="#fff"/>` +
        `<circle cx="5.6" cy="8" r="2.3" fill="#000"/><rect x="7" y="7.2" width="6" height="1.6" fill="#000"/>` +
        `<rect x="10.6" y="8" width="1.6" height="2.4" fill="#000"/></mask>` +
        `<rect width="16" height="16" rx="${rx}" fill="currentColor" mask="url(#amm-${id})"/></symbol>`,
    )
  }
  return `<svg class="sprite" aria-hidden="true" focusable="false">${syms.join('')}</svg>`
}

const icon = (k, cls = '') => `<svg class="i ${cls}" aria-hidden="true" focusable="false"><use href="#i-${k}"/></svg>`
const amark = (id) => `<svg class="am" aria-hidden="true" focusable="false"><use href="#am-${id}"/></svg>`

/** The wordmark sketches. Colours come from the custom properties of the surrounding scope. */
function wordmark(id, label = 'Ruum') {
  const a = `role="img" aria-label="${label}"`
  if (id === 'paper') {
    return (
      `<svg class="wm" ${a} viewBox="4 13 178 54" fill="none" stroke="var(--primary)" stroke-width="6.5" stroke-linecap="round" stroke-linejoin="round">` +
      '<path d="M12 20V60M12 37C12 26 19 20 30 20"/>' +
      '<path d="M42 20V44A14 14 0 0 0 70 44M70 20V60"/>' +
      '<path d="M84 20V44A14 14 0 0 0 112 44M112 20V60"/>' +
      '<path d="M126 60V20M126 34A12 12 0 0 1 150 34V60M150 34A12 12 0 0 1 174 34V60"/></svg>'
    )
  }
  if (id === 'marker') {
    return (
      `<svg class="wm" ${a} viewBox="2 2 202 74" fill="none">` +
      '<path d="M48 42L136 35L139 67L51 72Z" fill="var(--wm-hl)"/>' +
      '<g stroke="var(--text)" stroke-width="11" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M14 62V12H29A13 13 0 0 1 29 38H14M29 38L42 62"/>' +
      '<path d="M56 24V46A14 14 0 0 0 84 46M84 24V62"/>' +
      '<path d="M98 24V46A14 14 0 0 0 126 46M126 24V62"/>' +
      '<path d="M140 62V24M140 37A12 12 0 0 1 164 37V62M164 37A12 12 0 0 1 188 37V62"/></g></svg>'
    )
  }
  return (
    `<svg class="wm" ${a} viewBox="0 0 184 80" fill="none" stroke-linecap="square" stroke-linejoin="miter">` +
    '<path d="M3 15V3H15M169 3H181V15M181 65V77H169M15 77H3V65" stroke="var(--primary)" stroke-width="3"/>' +
    '<g stroke="var(--text)" stroke-width="6">' +
    '<path d="M18 60V22H36"/><path d="M48 22V60H72V22"/><path d="M86 22V60H110V22"/>' +
    '<path d="M124 60V22H166V60M145 22V60"/></g></svg>'
  )
}

/** A small app icon (favicon and tab icon) sketch for each direction. */
function appIcon(id) {
  const a = 'role="img" aria-label="App icon sketch"'
  if (id === 'paper') {
    return (
      `<svg class="appicon" ${a} viewBox="0 0 48 48"><rect width="48" height="48" rx="12" fill="var(--primary)"/>` +
      '<path d="M17 35V14M17 23C17 17.5 21 14 29 14" fill="none" stroke="var(--on-primary)" stroke-width="4.5" stroke-linecap="round"/><circle cx="33" cy="33" r="2.6" fill="var(--on-primary)"/></svg>'
    )
  }
  if (id === 'marker') {
    return (
      `<svg class="appicon" ${a} viewBox="0 0 48 48"><rect x="4" y="4" width="43" height="43" rx="12" fill="var(--text)"/>` +
      '<rect x="1" y="1" width="42" height="42" rx="12" fill="#ffd84d" stroke="var(--text)" stroke-width="2"/>' +
      '<path d="M15 33V11H22A6 6 0 0 1 22 23H15M22 23L28 33" fill="none" stroke="#1c1b17" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round"/></svg>'
    )
  }
  return (
    `<svg class="appicon" ${a} viewBox="0 0 48 48"><rect x="1.5" y="1.5" width="45" height="45" rx="3" fill="var(--surface)" stroke="var(--primary)" stroke-width="3"/>` +
    '<path d="M12 34V15H20M24 15V34H36V15" fill="none" stroke="var(--text)" stroke-width="4" stroke-linecap="square"/></svg>'
  )
}

// ---------------------------------------------------------------------------------------------
// The three mock screens (the same content in every direction; copy follows docs/ux/copy.md)
// ---------------------------------------------------------------------------------------------

function topbar(id) {
  return `<div class="tb"><span class="tb-wm">${wordmark(id)}</span>
    <span class="tb-space">Pärn Studio ${icon('chevron', 'i-sm')}</span>
    <span class="tb-search">${icon('search', 'i-sm')}<span>Search</span></span>
    <span class="av av-me" aria-hidden="true">LT</span></div>`
}

function rail(active) {
  const items = [['folder', 'Files'], ['share', 'Shared'], ['trash', 'Trash'], ['key', 'Agents']]
  return `<nav class="rail" aria-label="Main (mock)">${items
    .map(([i, t]) => `<span class="nav${t === active ? ' nav-on' : ''}">${icon(i)}<span>${t}</span></span>`)
    .join('')}</nav>`
}

function mockFiles(id) {
  const agent = `<span class="agent">${amark(id)}Claude Desktop</span> <span class="for">for Liis Tamm</span>`
  const rows = [
    { k: 'folder', n: 'Moodboard', when: 'Yesterday at 14:00', who: 'Jonas Kask', size: '' },
    { k: 'file', n: 'Brief.pdf', when: '2 hours ago', who: agent, size: '2.4 MB', sel: true, tag: true },
    { k: 'image', n: 'Logo v3.png', when: '5 Nov 2026', who: 'Liis Tamm', size: '860 KB' },
    { k: 'file', n: 'Invoice 0142.pdf', when: '3 Nov 2026', who: 'Jonas Kask', size: '120 KB' },
  ]
  const tr = rows
    .map(
      (r) => `<tr class="${r.sel ? 'sel' : ''}"><td class="c-chk"><span class="chk${r.sel ? ' chk-on' : ''}">${r.sel ? icon('check') : ''}</span></td>
      <td class="c-name">${icon(r.k, 'kind')}<span class="nm"><span class="nm-t">${r.n}</span>${r.tag ? '<span class="dot" title="Tag: Client"></span>' : ''}
      <span class="sub">${r.when}, ${r.who}</span></span></td>
      <td class="c-when"><span class="t-num">${r.when}</span>, ${r.who}</td><td class="c-size t-num">${r.size}</td></tr>`,
    )
    .join('')
  return `${topbar(id)}<div class="app">${rail('Files')}<div class="main">
    <div class="crumb">Files / Clients /</div>
    <div class="head"><h4 class="h1"><span class="hl">Pärn</span></h4><span class="btn btn-p">${icon('upload', 'i-sm')}Upload</span></div>
    <div class="chips"><span class="chip">${amark(id)}Changed by agents</span><span class="chip chip-q">1 selected</span></div>
    <table class="fl"><thead><tr><th class="c-chk"></th><th>Name</th><th class="c-when">Changed</th><th class="c-size">Size</th></tr></thead>
    <tbody>${tr}</tbody></table>
    <div class="toast">${icon('check', 'i-sm ok')}<span><b>Brief.pdf</b> is up to date. Claude Desktop saved a new version.</span></div>
    </div></div>`
}

function mockShare(id) {
  return `<div class="scrim"><div class="dlg" role="group" aria-label="Share dialog (mock)">
    <div class="dlg-h"><h4 class="h2">Share Brief.pdf</h4><span class="ib">${icon('close')}</span></div>
    <div class="lbl">Add people</div>
    <div class="addrow"><span class="field">Name or email</span><span class="select">Can edit ${icon('chevron', 'i-sm')}</span><span class="btn btn-p">Share</span></div>
    <div class="lbl">People with access</div>
    <ul class="people">
      <li><span class="av">LT</span><span class="pp"><span class="pn">Liis Tamm</span><span class="pe">liis@parn.studio</span></span><span class="role-t">Owner</span></li>
      <li><span class="av av-2">JK</span><span class="pp"><span class="pn">Jonas Kask</span><span class="pe">jonas@parn.studio</span></span><span class="select select-s">Can edit ${icon('chevron', 'i-sm')}</span></li>
      <li><span class="av av-a">${amark(id)}</span><span class="pp"><span class="pn agent">Claude Desktop</span><span class="pe">An agent for Liis Tamm, from folder <u>Pärn</u></span></span><span class="role-t">Can edit</span></li>
    </ul>
    <div class="linkrow"><span class="sw sw-on" aria-hidden="true"><span></span></span><span class="pn">Anyone with the link</span><span class="select select-s">Can view ${icon('chevron', 'i-sm')}</span></div>
    <div class="note">${icon('link', 'i-sm')}<span>Copy the link now. For safety, Ruum shows it only once.</span></div>
    <div class="dlg-f"><span class="btn btn-s">${icon('link', 'i-sm')}Copy link</span><span class="btn btn-p">Done</span></div>
  </div></div>`
}

function mockAgents(id) {
  const card = (name, badge, bcls, access, used, extra = '') => `<article class="card">
    <div class="card-h"><span class="agent card-n">${amark(id)}<u>${name}</u></span><span class="badge ${bcls}">${badge}</span></div>
    <p class="card-a">${access}</p><p class="card-u">${used}</p>${extra}</article>`
  return `${topbar(id)}<div class="app">${rail('Agents')}<div class="main">
    <div class="head"><h4 class="h1"><span class="hl">Agents</span></h4><span class="btn btn-p">${icon('plus', 'i-sm')}New agent</span></div>
    <p class="lede">Agents work with the keys you give them. You see everything they change.</p>
    <div class="cards">
    ${card('Claude Desktop', 'Active', 'b-ok', 'Can edit in Clients/Pärn, until 4 Feb 2027', 'Last used 2 minutes ago')}
    ${card('Invoice helper', 'Paused', 'b-warn', 'Can view in Invoices', 'Last used 3 days ago', `<p class="warn">Paused. Its keys and connections don't work until you resume it.</p>`)}
    ${card('Night sorter', 'Active', 'b-ok', 'Can edit in Inbox, no end date', 'Not used yet')}
    </div></div></div>`
}

const screens = [
  ['files', 'File browser with an agent attribution', mockFiles],
  ['share', 'Share dialog', mockShare],
  ['agents', 'Agents page', mockAgents],
]

function mock(id, mode, fn, title) {
  return `<figure class="mf"><figcaption>${mode === 'light' ? 'Light' : 'Dark'}</figcaption>
    <div class="mk s-${id} p-${id}-${mode} m-${mode}" aria-label="${title}, ${meta[id].name}, ${mode} (mock)">${fn(id)}</div></figure>`
}

// ---------------------------------------------------------------------------------------------
// Palette swatches and contrast facts
// ---------------------------------------------------------------------------------------------

const shown = [
  ['canvas', 'Page'], ['surface', 'Cards'], ['sunken', 'Top bar'], ['text', 'Text'], ['text-muted', 'Muted'],
  ['primary', 'Primary'], ['primary-soft', 'Primary soft'], ['agent', 'Agent'], ['agent-soft', 'Agent soft'],
  ['danger', 'Danger'], ['success', 'Success'], ['warning', 'Warning'],
]
// The pair shown under each swatch: [foreground, background].
const proof = {
  canvas: ['text', 'canvas'], surface: ['text-muted', 'surface'], sunken: ['text', 'sunken'], text: ['text', 'surface'],
  'text-muted': ['text-muted', 'canvas'], primary: ['on-primary', 'primary'], 'primary-soft': ['primary', 'primary-soft'],
  agent: ['agent', 'surface'], 'agent-soft': ['agent', 'agent-soft'], danger: ['danger', 'surface'],
  success: ['success', 'success-soft'], warning: ['warning', 'warning-soft'],
}

function swatches(id, mode) {
  const t = directions[id][mode]
  return `<div class="sw-grid">${shown
    .map(([k, label]) => {
      const [fg, bg] = proof[k]
      const r = ratio(t[fg], t[bg])
      return `<div class="swc"><div class="chipc" style="background:${t[k]}"><span style="color:${t[fg]};background:${t[bg]}" class="aa">Aa</span></div>
      <div class="swt"><b>${label}</b><code>${t[k]}</code><span class="rt">${fg} on ${bg} ${r.toFixed(1)}:1</span></div></div>`
    })
    .join('')}</div>`
}

function fullTable(id) {
  const L = directions[id].light
  const D = directions[id].dark
  return `<table class="tt"><thead><tr><th>Token</th><th>Light</th><th>Dark</th></tr></thead><tbody>${Object.keys(L)
    .map(
      (k) => `<tr><td><code>--color-${k}</code></td><td><span class="mini" style="background:${L[k]}"></span><code>${L[k]}</code></td>
      <td><span class="mini" style="background:${D[k]}"></span><code>${D[k]}</code></td></tr>`,
    )
    .join('')}</tbody></table>`
}

// ---------------------------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------------------------

function paletteCss() {
  const out = []
  for (const id of ids) {
    for (const mode of ['light', 'dark']) {
      const t = directions[id][mode]
      const vars = Object.entries(t).map(([k, v]) => `--${k}:${v}`)
      vars.push(`--wm-hl:${id === 'marker' ? (mode === 'light' ? '#ffd84d' : '#6b5a10') : 'transparent'}`)
      out.push(`.p-${id}-${mode}{${vars.join(';')}}`)
    }
    out.push(`.s-${id}{${Object.entries(meta[id].css).map(([k, v]) => `${k}:${v}`).join(';')}}`)
  }
  return out.join('\n')
}

const css = `
:root{--bg:#f6f6f4;--fg:#1b1b1a;--muted:#55554f;--line:#dcdcd6;--card:#ffffff;--code:#ececE8;color-scheme:light dark}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){--bg:#121212;--fg:#ececea;--muted:#a9a9a3;--line:#33332f;--card:#1b1b1a;--code:#262624}}
:root[data-theme="dark"]{--bg:#121212;--fg:#ececea;--muted:#a9a9a3;--line:#33332f;--card:#1b1b1a;--code:#262624}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 ${SYS}}
.sprite{position:absolute;width:0;height:0;overflow:hidden}
.wrap{max-width:1240px;margin:0 auto;padding:32px 24px 64px}
@media (max-width:640px){.wrap{padding:20px 16px 48px}}
h1{font-size:30px;line-height:1.2;margin:0 0 8px}
h2{font-size:24px;line-height:1.25;margin:0}
h3{font-size:18px;margin:32px 0 12px}
p{max-width:72ch}
.muted{color:var(--muted)}
code{font:13px/1.4 ${MONO}}
a{color:inherit}
.intro{margin-bottom:24px}
.toc{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:16px;margin:24px 0 48px}
.toc a{display:block;text-decoration:none;border:1px solid var(--line);border-radius:12px;overflow:hidden;background:var(--card)}
.toc a:focus-visible{outline:3px solid currentColor;outline-offset:2px}
.toc .tw{padding:20px 20px 12px;background:var(--canvas)}
.toc .tw .wm{height:40px;width:auto;display:block}
.toc .tx{padding:12px 20px 16px}
.toc .tx b{display:block;font-size:17px}
.toc .tx span{color:var(--muted);font-size:14px}
section.dir{border-top:1px solid var(--line);padding-top:40px;margin-top:48px}
.dir-h{display:flex;flex-wrap:wrap;align-items:baseline;gap:8px 16px}
.dir-h .num{font:600 14px/1 ${SYS};color:var(--muted)}
.dir-h .nick{color:var(--muted)}
.facts{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:16px}
.box{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px}
.box h4{margin:0 0 8px;font-size:15px}
.box p{margin:0 0 8px;font-size:14px;color:var(--muted)}
.marks{display:grid;grid-template-columns:1fr 1fr;gap:12px}
@media (max-width:480px){.marks{grid-template-columns:1fr}}
.markt{border-radius:10px;padding:20px 16px;background:var(--canvas);display:flex;align-items:center;gap:16px;border:1px solid var(--border)}
.markt .wm{height:48px;width:auto;max-width:65%}
.appicon{width:40px;height:40px;flex:none}
.spec{background:var(--canvas);color:var(--text);border-radius:10px;padding:16px;border:1px solid var(--border)}
.spec .s1{font:var(--hw) 28px/1.15 var(--fh);margin:0 0 6px}
.spec .s2{font:400 16px/1.5 var(--fb);margin:0 0 6px;color:var(--text)}
.spec .s3{font:400 13px/1.4 var(--fm);color:var(--text-muted);margin:0}
.spec dl{margin:12px 0 0;font:13px/1.45 ${SYS};color:var(--text-muted);display:grid;grid-template-columns:auto 1fr;gap:2px 10px}
.spec dt{font-weight:600;color:var(--text)}
.spec dd{margin:0}
.icons{display:flex;flex-wrap:wrap;gap:6px;background:var(--canvas);color:var(--text);border-radius:10px;padding:12px;border:1px solid var(--border)}
.icons .ic{display:grid;place-items:center;width:40px;height:40px;border-radius:var(--r-md)}
.icons .ic .i{width:22px;height:22px}
.icons .ic-on{background:var(--hl,transparent)}
.s-marker .icons .ic-on{background:var(--primary-soft)}
.icons .ic-a{color:var(--agent)}
.icons .ic-a .am{width:20px;height:20px}
.pal{display:grid;grid-template-columns:1fr 1fr;gap:16px}
@media (max-width:900px){.pal{grid-template-columns:1fr}}
.palw{border-radius:12px;padding:12px;background:var(--canvas);color:var(--text);border:1px solid var(--border)}
.palw h4{margin:0 0 8px;font:600 14px/1.3 ${SYS}}
.sw-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:8px}
.swc{display:flex;gap:8px;align-items:center;min-width:0}
.chipc{width:44px;height:44px;border-radius:8px;flex:none;display:grid;place-items:center;border:1px solid var(--border)}
.aa{font:600 12px/1 ${SYS};padding:3px 4px;border-radius:4px}
.swt{min-width:0;font:12px/1.35 ${SYS}}
.swt b{display:block;font-size:13px}
.swt code{display:block;font-size:12px;color:var(--text-muted)}
.swt .rt{display:block;color:var(--text-muted);overflow-wrap:anywhere}
details{margin-top:12px}
summary{cursor:pointer;font-weight:600}
.tt{border-collapse:collapse;margin-top:8px;font-size:13px;width:100%;max-width:560px}
.tt th,.tt td{text-align:left;padding:4px 8px;border-bottom:1px solid var(--line)}
.mini{display:inline-block;width:14px;height:14px;border-radius:3px;vertical-align:-2px;margin-right:6px;border:1px solid var(--line)}
.scr h3{margin-top:28px}
.pair{display:grid;grid-template-columns:1fr 1fr;gap:16px;align-items:start}
@media (max-width:1060px){.pair{grid-template-columns:1fr}}
figure.mf{margin:0;min-width:0}
figure.mf figcaption{font:600 13px/1.3 ${SYS};color:var(--muted);margin:0 0 6px}
.check{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px;margin-top:16px;font-size:14px}
.check ul{margin:8px 0 0;padding-left:20px}
.choose{border-top:1px solid var(--line);margin-top:56px;padding-top:40px}
.choose ol li{margin-bottom:8px;max-width:72ch}
.rec{background:var(--card);border:1px solid var(--line);border-left:4px solid var(--fg);border-radius:12px;padding:16px 20px;max-width:80ch}

/* ---- The mock screens: only the custom properties of the direction and theme ---- */
.mk{container-type:inline-size;background:var(--canvas);color:var(--text);font:400 14px/1.45 var(--fb);border:1px solid var(--border);border-radius:var(--r-lg);overflow:hidden;min-width:0}
.mk .i{width:20px;height:20px;fill:none;stroke:currentColor;stroke-width:var(--isw);stroke-linecap:var(--cap);stroke-linejoin:var(--join);flex:none}
.mk .i-sm{width:16px;height:16px}
.mk .am{width:15px;height:15px;flex:none;vertical-align:-2px;margin-right:4px}
.mk .tb{display:flex;align-items:center;gap:12px;background:var(--sunken);padding:8px 12px;border-bottom:1px solid var(--border);min-width:0}
.mk .tb-wm .wm{height:22px;width:auto;display:block}
.mk .tb-space{display:inline-flex;align-items:center;gap:4px;font-weight:600;white-space:nowrap;padding:4px 10px;border-radius:999px;background:var(--surface);border:1px solid var(--border)}
.mk .tb-search{flex:1;min-width:0;max-width:220px;margin-left:auto;display:flex;align-items:center;gap:6px;color:var(--text-faint);background:var(--surface);border:1px solid var(--border-control);border-radius:var(--r-md);padding:4px 10px}
.mk .av{display:inline-grid;place-items:center;width:28px;height:28px;border-radius:999px;background:var(--primary-soft);color:var(--primary);font:600 12px/1 var(--fb);flex:none}
.mk .av-2{background:var(--sunken);color:var(--text)}
.mk .av-a{background:var(--agent-soft);color:var(--agent)}
.mk .av-a .am{margin:0}
.mk .app{display:flex;min-width:0}
.mk .rail{width:128px;flex:none;padding:12px 8px;display:flex;flex-direction:column;gap:2px;border-right:1px solid var(--border);background:var(--canvas)}
.mk .nav{display:flex;align-items:center;gap:8px;padding:6px 8px;border-radius:var(--r-md);color:var(--text-muted);border-left:3px solid transparent}
.mk .nav-on{background:var(--primary-soft);color:var(--primary);font-weight:600;border-left-color:var(--primary)}
.s-marker .mk .nav-on,.mk.s-marker .nav-on{background:var(--primary-soft);color:var(--text);border-left-color:var(--text)}
.mk .main{flex:1;min-width:0;padding:14px 16px 16px}
.mk .crumb{color:var(--text-muted);font-size:13px}
.mk .head{display:flex;align-items:center;justify-content:space-between;gap:12px;margin:2px 0 10px}
.mk .h1{font:var(--hw) 24px/1.2 var(--fh);margin:0;letter-spacing:-.01em}
.mk .h2{font:var(--hw) 19px/1.25 var(--fh);margin:0}
.mk.s-marker .hl{background:linear-gradient(transparent 58%,var(--primary-soft) 58%);padding:0 2px}
.mk .btn{display:inline-flex;align-items:center;gap:6px;font:600 14px/1 var(--fb);padding:9px 14px;border-radius:var(--r-md);border:var(--btn-bw) solid var(--border-control);background:var(--surface);color:var(--text);white-space:nowrap}
.mk .btn-p{background:var(--primary);color:var(--on-primary);border-color:var(--primary);box-shadow:var(--btn-shadow)}
.mk.s-marker.m-dark .btn-p{box-shadow:3px 3px 0 #000}
.mk.s-marker .btn{border-color:var(--text)}
.mk .chips{display:flex;flex-wrap:wrap;gap:6px;margin-bottom:10px}
.mk .chip{display:inline-flex;align-items:center;font-size:13px;padding:3px 10px;border-radius:999px;border:1px solid var(--border-control);background:var(--surface);color:var(--agent)}
.mk .chip-q{color:var(--text-muted);border-color:var(--border)}
.mk .fl{width:100%;border-collapse:separate;border-spacing:0;background:var(--surface);border:1px solid var(--border);border-radius:var(--r-lg);overflow:hidden}
.mk .fl th{font-size:12px;font-weight:600;text-align:left;color:var(--text-muted);background:var(--sunken);padding:7px 10px;border-bottom:1px solid var(--border)}
.mk.s-graphite .fl th{font-family:var(--fm);font-weight:500}
.mk .fl td{padding:9px 10px;border-bottom:1px solid var(--border);vertical-align:middle}
.mk .fl tr:last-child td{border-bottom:0}
.mk .fl tr.sel td{background:var(--selected)}
.mk .c-chk{width:30px;padding-right:0}
.mk .chk{display:grid;place-items:center;width:16px;height:16px;border:1.5px solid var(--border-control);border-radius:var(--r-sm);background:var(--surface)}
.mk .chk-on{background:var(--primary);border-color:var(--primary);color:var(--on-primary)}
.mk .chk .i{width:12px;height:12px;stroke-width:2.5}
.mk .c-name{display:flex;align-items:flex-start;gap:8px}
.mk .kind{color:var(--text-muted);margin-top:1px}
.mk .nm{min-width:0}
.mk .nm-t{font-weight:500}
.mk .dot{display:inline-block;width:8px;height:8px;border-radius:999px;background:var(--primary);margin-left:6px;vertical-align:1px}
.mk .sub{display:none;font-size:12px;color:var(--text-muted)}
.mk .c-when{color:var(--text-muted);font-size:13px}
.mk .c-size{color:var(--text-muted);font-size:13px;text-align:right;white-space:nowrap}
.mk.s-graphite .t-num{font-family:var(--fm);font-size:12px}
.mk .agent{color:var(--agent);font-weight:500;white-space:nowrap}
.mk .for{color:inherit}
.mk .toast{display:flex;gap:8px;align-items:flex-start;margin-top:12px;background:var(--surface);border:1px solid var(--border);border-radius:var(--r-lg);padding:10px 12px;box-shadow:0 8px 24px rgb(0 0 0 / .10);font-size:13px}
.mk .ok{color:var(--success);margin-top:1px}
@container (max-width:520px){
  .mk .rail{display:none}
  .mk .c-when,.mk .c-size{display:none}
  .mk .sub{display:block}
  .mk .tb-space{display:none}
  .mk .agent{white-space:normal}
}
/* Share dialog */
.mk .scrim{padding:20px;background:color-mix(in srgb,var(--text) 22%,var(--canvas))}
.mk.m-dark .scrim{background:color-mix(in srgb,#000 45%,var(--canvas))}
.mk .dlg{background:var(--surface);border-radius:var(--r-lg);padding:16px;max-width:520px;margin:0 auto;box-shadow:0 8px 24px rgb(0 0 0 / .18);border:1px solid var(--border)}
.mk.s-marker .dlg{border:2px solid var(--text);box-shadow:5px 5px 0 var(--text)}
.mk.s-marker.m-dark .dlg{box-shadow:5px 5px 0 #000}
.mk .dlg-h{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.mk .ib{display:grid;place-items:center;width:32px;height:32px;border-radius:var(--r-md);color:var(--text-muted)}
.mk .lbl{font-weight:600;font-size:13px;margin:12px 0 6px}
.mk .addrow{display:flex;gap:8px;flex-wrap:wrap}
.mk .field{flex:1 1 140px;min-width:0;border:1px solid var(--border-control);border-radius:var(--r-md);padding:8px 10px;color:var(--text-faint);background:var(--surface)}
.mk .select{display:inline-flex;align-items:center;gap:4px;border:1px solid var(--border-control);border-radius:var(--r-md);padding:8px 10px;background:var(--surface);white-space:nowrap}
.mk .select-s{padding:5px 8px;font-size:13px}
.mk .people{list-style:none;margin:0;padding:0}
.mk .people li{display:flex;align-items:center;gap:10px;padding:7px 0;border-bottom:1px solid var(--border)}
.mk .pp{flex:1;min-width:0;display:flex;flex-direction:column}
.mk .pn{font-weight:500}
.mk .pe{font-size:12px;color:var(--text-muted)}
.mk .pe u{color:var(--link)}
.mk .role-t{font-size:13px;color:var(--text-muted);white-space:nowrap}
.mk .linkrow{display:flex;align-items:center;gap:10px;margin-top:14px}
.mk .linkrow .pn{flex:1}
.mk .sw{width:34px;height:20px;border-radius:999px;background:var(--border-control);position:relative;flex:none}
.mk .sw span{position:absolute;top:3px;left:3px;width:14px;height:14px;border-radius:999px;background:var(--surface)}
.mk .sw-on{background:var(--primary)}
.mk .sw-on span{left:17px;background:var(--on-primary)}
.mk.s-graphite .sw,.mk.s-graphite .sw span{border-radius:2px}
.mk .note{display:flex;gap:8px;align-items:flex-start;margin-top:10px;background:var(--primary-soft);color:var(--primary);border-left:3px solid var(--primary);border-radius:var(--r-sm);padding:8px 10px;font-size:13px}
.mk.s-marker .note{color:var(--text);border-left-color:var(--text)}
.mk.s-marker.m-dark .note{color:var(--text)}
.mk .dlg-f{display:flex;justify-content:flex-end;gap:8px;margin-top:14px;flex-wrap:wrap}
/* Agents page */
.mk .lede{margin:0 0 12px;color:var(--text-muted);font-size:13px}
.mk .cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(190px,1fr));gap:10px}
.mk .card{background:var(--surface);border:1px solid var(--border);border-radius:var(--r-lg);padding:12px}
.mk.s-marker .card{border:2px solid var(--text)}
.mk .card-h{display:flex;justify-content:space-between;align-items:center;gap:8px}
.mk .card-n{font:var(--hw) 15px/1.3 var(--fh);white-space:normal}
.mk .card-n u{text-underline-offset:2px}
.mk .badge{font-size:12px;font-weight:600;padding:2px 8px;border-radius:var(--r-sm);white-space:nowrap}
.mk .b-ok{background:var(--success-soft);color:var(--success)}
.mk .b-warn{background:var(--warning-soft);color:var(--warning)}
.mk .card-a{margin:8px 0 2px;font-size:13px}
.mk .card-u{margin:0;font-size:12px;color:var(--text-muted)}
.mk .warn{margin:8px 0 0;font-size:12px;background:var(--warning-soft);color:var(--warning);border-radius:var(--r-sm);padding:6px 8px}
`

// ---------------------------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------------------------

const results = check()
const failed = results.filter((r) => !r.ok)

function checkSummary(id) {
  const rs = results.filter((r) => r.dir === id)
  const text = rs.filter((r) => r.min === 4.5)
  const low = (theme) => text.filter((r) => r.theme === theme).reduce((a, b) => (b.r < a.r ? b : a))
  const L = low('light')
  const D = low('dark')
  return `<div class="check"><b>Contrast check:</b> ${rs.filter((r) => r.ok).length} of ${rs.length} pairs pass WCAG 2.2 AA
    (the pairs of <code>docs/ux/tools/contrast.mjs</code>, plus the 8 tag colours on the new surface).
    <ul><li>Lowest text pair, light: ${L.fg} on ${L.bg}, ${L.r.toFixed(2)}:1 (needs 4.5)</li>
    <li>Lowest text pair, dark: ${D.fg} on ${D.bg}, ${D.r.toFixed(2)}:1 (needs 4.5)</li></ul></div>`
}

function section(id) {
  const m = meta[id]
  return `<section class="dir" id="${id}" aria-labelledby="${id}-h">
  <div class="dir-h"><span class="num">Direction ${m.n}</span><h2 id="${id}-h">${m.name}</h2><span class="nick">${m.nick}</span></div>
  <p>${m.character}</p>

  <h3>Wordmark and app icon</h3>
  <div class="marks s-${id}">
    <div class="markt p-${id}-light">${wordmark(id, `Ruum wordmark, ${m.name}, light`)}${appIcon(id)}</div>
    <div class="markt p-${id}-dark">${wordmark(id, `Ruum wordmark, ${m.name}, dark`)}${appIcon(id)}</div>
  </div>
  <p class="muted">${m.wordmark}</p>

  <h3>Type and icons</h3>
  <div class="facts s-${id} p-${id}-light">
    <div class="spec"><p class="s1">Pärn Studio, all in one room</p>
      <p class="s2">Claude Desktop changed Brief.pdf 2 hours ago. You can go back to any version.</p>
      <p class="s3">2.4 MB · 14:02 · key ends in 7Hq2</p>
      <dl><dt>${m.type.head}</dt><dd>${m.type.headNote}</dd><dt>${m.type.body}</dt><dd>${m.type.bodyNote}</dd><dt>${m.type.mono}</dt><dd>${m.type.monoNote}</dd></dl></div>
    <div><div class="icons">${['folder', 'file', 'image', 'share', 'search', 'upload', 'link', 'trash', 'key', 'more', 'close', 'check']
      .map((k, i) => `<span class="ic${i === 0 ? ' ic-on' : ''}">${icon(k)}</span>`)
      .join('')}<span class="ic ic-a" title="Agent mark">${amark(id)}</span></div>
      <p class="muted" style="font-size:14px">${m.icons}</p></div>
  </div>

  <h3>Palette</h3>
  <div class="pal">
    <div class="palw p-${id}-light"><h4>Light</h4>${swatches(id, 'light')}</div>
    <div class="palw p-${id}-dark"><h4>Dark</h4>${swatches(id, 'dark')}</div>
  </div>
  <details><summary>All ${Object.keys(directions[id].light).length} colour tokens</summary>${fullTable(id)}</details>
  ${checkSummary(id)}

  <div class="scr">${screens
    .map(([, title, fn]) => `<h3>${title}</h3><div class="pair">${mock(id, 'light', fn, title)}${mock(id, 'dark', fn, title)}</div>`)
    .join('')}</div>
  </section>`
}

const fonts =
  'https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wght@12..96,600;12..96,700&family=Figtree:wght@400;500;600' +
  '&family=Fraunces:opsz,wght@9..144,600&family=IBM+Plex+Mono:wght@400;500&family=IBM+Plex+Sans:wght@400;500;600&display=swap'

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Ruum brand directions</title>
<meta name="description" content="Three brand directions for Ruum to choose from: palette, type, icons, wordmark and the same three screens in light and dark.">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="${fonts.replaceAll('&', '&amp;')}">
<style>
${paletteCss()}
${css}
/* The icon row uses the stroke of its direction too. */
.icons .i{fill:none;stroke:currentColor;stroke-width:var(--isw);stroke-linecap:var(--cap);stroke-linejoin:var(--join)}
.icons .am{fill:currentColor}
</style>
</head>
<body>
${sprite()}
<main class="wrap">
  <header class="intro">
    <h1>Ruum brand directions</h1>
    <p>Three directions for the look of Ruum, for you to choose one. Each has a character, a light
    and a dark palette (every text pair passes WCAG 2.2 AA), a type pairing, an icon style, a wordmark
    sketch and the same three screens: the file browser with an agent's change, the share dialog
    and the agents page. The words on the screens follow the voice of <code>docs/ux/copy.md</code>:
    informal and playful, calm where data, security or money are at stake.</p>
    <p class="muted">Nothing here changes the product. A chosen direction is applied later through the
    token files only; <code>docs/ux/brand/README.md</code> says which tokens change. The mock screens
    are drawings, not the product; the sizes are compact so that light and dark fit side by side.</p>
  </header>
  <nav class="toc" aria-label="Directions">${ids
    .map(
      (id) => `<a href="#${id}" class="s-${id} p-${id}-light"><div class="tw">${wordmark(id, meta[id].name)}</div>
      <div class="tx"><b>${meta[id].n}. ${meta[id].name}</b><span>${meta[id].line}</span></div></a>`,
    )
    .join('')}</nav>

  ${ids.map(section).join('\n')}

  <section class="choose" id="choose" aria-labelledby="choose-h">
    <h2 id="choose-h">How to choose</h2>
    <p>Ask three questions, in this order.</p>
    <ol>
      <li><b>Who should feel at home first?</b> Small studios and offices that keep their work in Ruum
      (Paper and spruce, Marker), or developers and IT people who connect agents to it (Graphite).</li>
      <li><b>Where should the playfulness live?</b> Only in the words (Paper and spruce, Graphite), or
      in the words and the pictures (Marker).</li>
      <li><b>How much change now?</b> Paper and spruce changes no colour and adds one self-hosted font
      (Fraunces, for titles). Marker and Graphite change every colour token and add two self-hosted
      font families.</li>
    </ol>
    <div class="rec">
      <p><b>My recommendation: 1, Paper and spruce.</b></p>
      <p>Ruum's promise is "a friendly colleague who also keeps your work safe". The voice already
      carries the friendly part, and it does it well. The visuals should carry the safe part: warm,
      quiet, and the same on every screen, so a security or delete dialog never looks like a joke.
      Paper and spruce does that, keeps the agents' clay colour that people already learn as
      "an agent did this", passes every contrast pair today, and needs no colour change. The soft serif
      in titles and the round wordmark add the character that the current system font lacks.</p>
      <p>Choose <b>Marker</b> if you want Ruum to be recognisable from across the room and the
      playful voice to show before anyone reads a word; it costs more care on calm screens (the
      yellow must stay off errors, security and billing). Choose <b>Graphite</b> if the first buyers
      are developers and IT, and you accept a look that is cooler than the voice.</p>
    </div>
  </section>
</main>
</body>
</html>
`

fs.writeFileSync(path.join(here, 'directions.html'), html)
console.log(`directions.html: ${(html.length / 1024).toFixed(0)} KB, ${results.length} pairs, ${failed.length} fail`)
for (const f of failed) console.log(`FAIL ${f.dir} ${f.theme} ${f.fg} on ${f.bg} ${f.r.toFixed(2)} < ${f.min}`)

if (process.argv.includes('--md')) {
  for (const id of ids) {
    console.log(`\n${meta[id].name}\n\n| Token | Light | Dark |\n|---|---|---|`)
    for (const k of Object.keys(directions[id].light)) {
      console.log(`| \`${k}\` | \`${directions[id].light[k]}\` | \`${directions[id].dark[k]}\` |`)
    }
  }
}
process.exit(failed.length ? 1 : 0)
