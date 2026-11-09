// Theme packages (design 12.6): load, check and merge a theme over the core tokens.
//
// A theme package is a folder with a `theme.json` manifest, token overrides in the W3C Design
// Tokens format (base, light, dark) and asset files (wordmark, favicon, app icons, fonts). A theme
// holds no code: it can only give new values to tokens that the core files of
// src/design/tokens/ already define, and add image and font files. The rules are here, so that
// the token build (build-tokens.mjs), the validator (check-theme.mjs) and the contrast check of
// docs/ux/tools/contrast.mjs apply the same ones.
//
// This module uses only Node built-ins: docs/ux/tools imports it without the web packages.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

export const web = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
export const coreDir = path.join(web, 'src/design/tokens')
export const themesDir = path.join(web, 'src/design/themes')

/** The theme that the token build applies over the core tokens (decision 0005 B-5). */
export const DEFAULT_THEME = 'graphite'

/** The colour schemes. Every theme has both. */
export const schemes = ['light', 'dark']

/** The core token files: `base` is the same in every scheme, the colour files per scheme. */
export const coreFiles = { base: 'base.tokens.json', light: 'color.light.tokens.json', dark: 'color.dark.tokens.json' }

/** Limits of a package (bytes). */
export const limits = {
  manifest: 64 * 1024,
  tokens: 256 * 1024,
  svg: 64 * 1024,
  png: 512 * 1024,
  woff2: 256 * 1024,
  text: 128 * 1024,
  total: 4 * 1024 * 1024,
  files: 64,
}

/** The file types a package may hold, by extension. Everything else is refused. */
export const fileTypes = {
  '.json': 'json',
  '.svg': 'svg',
  '.png': 'png',
  '.woff2': 'woff2',
  '.txt': 'text',
  '.md': 'text',
}

/** The unicode ranges of the font subsets a theme may declare (the ranges of the Fontsource builds). */
export const fontSubsets = {
  latin:
    'U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+0304, U+0308, U+0329, U+2000-206F, U+20AC, U+2122, U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD',
  'latin-ext':
    'U+0100-02BA, U+02BD-02C5, U+02C7-02CC, U+02CE-02D7, U+02DD-02FF, U+0304, U+0308, U+0329, U+1D00-1DBF, U+1E00-1E9F, U+1EF2-1EFF, U+2020, U+20A0-20AB, U+20AD-20C0, U+2113, U+2C60-2C7F, U+A720-A7FF',
}

// [foreground, background, minimum ratio, what]. WCAG 2.2: 4.5:1 for text, 3:1 for control
// borders, the focus ring, icons, edges and tag colours. text-faint is only for surface, canvas and
// sunken: on selected and primary-soft it is under 4.5:1 (UX panel A10), so it is not used there.
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
  ...['spruce', 'moss', 'clay', 'brick', 'ochre', 'slate', 'plum', 'sand'].map((t) => [`tag-${t}`, 'surface', 3, `tag colour ${t} (edge and dot)`]),
]

const readJson = (file) => JSON.parse(fs.readFileSync(file, 'utf8'))

/** The core token trees: { base, light, dark }. */
export function loadCore() {
  return Object.fromEntries(Object.entries(coreFiles).map(([k, f]) => [k, readJson(path.join(coreDir, f))]))
}

const isToken = (node) => node && typeof node === 'object' && '$value' in node

/** Every token of a tree: [{ path: 'a.b.c', type, token }], with the group `$type` inherited. */
export function flatten(tree) {
  const out = []
  const walk = (node, prefix, type) => {
    const t = node.$type ?? type
    for (const [k, v] of Object.entries(node)) {
      if (k.startsWith('$') || !v || typeof v !== 'object') continue
      const p = prefix ? `${prefix}.${k}` : k
      if (isToken(v)) out.push({ path: p, type: v.$type ?? t, token: v })
      else walk(v, p, t)
    }
  }
  walk(tree, '', undefined)
  return out
}

/** A copy of `core` with the `$value` (and `$description`) of each token of `override` replaced. */
export function merge(core, override) {
  const out = structuredClone(core)
  if (!override) return out
  for (const { path: p, token } of flatten(override)) {
    let node = out
    for (const k of p.split('.')) node = node?.[k]
    if (!isToken(node)) continue // check() reports unknown tokens
    node.$value = structuredClone(token.$value)
    if (token.$description) node.$description = token.$description
  }
  return out
}

/** The flat colour map of a colour tree: `text-muted`, `tag-spruce`, ... -> `#rrggbb` (opaque only). */
export function colours(tree) {
  const out = {}
  for (const { path: p, token } of flatten({ color: tree.color })) {
    const v = token.$value
    if (v && typeof v === 'object' && v.hex && v.alpha === undefined) out[p.slice('color.'.length).replaceAll('.', '-')] = v.hex.toLowerCase()
  }
  return out
}

function luminance(hex) {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
  const l = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
}

/** The WCAG contrast ratio of two `#rrggbb` colours. */
export function ratio(a, b) {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p)
  return (x + 0.05) / (y + 0.05)
}

/** Every pair in every scheme: [{ scheme, fg, bg, min, what, r, ok }]. `trees` is { light, dark }. */
export function contrast(trees) {
  const out = []
  for (const scheme of schemes) {
    const c = colours(trees[scheme])
    for (const [fg, bg, min, what] of pairs) {
      if (!c[fg] || !c[bg]) {
        out.push({ scheme, fg, bg, min, what, r: 0, ok: false, missing: true })
        continue
      }
      const r = ratio(c[fg], c[bg])
      out.push({ scheme, fg, bg, min, what, r, ok: r >= min })
    }
  }
  return out
}

// ---------------------------------------------------------------------------------------------
// Token values: a theme may give a token only a value of its type, never free text.
// ---------------------------------------------------------------------------------------------

const hexRe = /^#[0-9a-fA-F]{6}$/
const refRe = /^\{([a-z0-9-]+(?:\.[a-z0-9-]+)*)\}$/
const familyRe = /^-?[A-Za-z0-9][A-Za-z0-9 -]{0,63}$/
const keywordRe = /^[a-z][a-z-]{0,31}$/
const isNum = (v) => typeof v === 'number' && Number.isFinite(v)

function dimension(v, units = ['px', 'rem']) {
  if (!v || typeof v !== 'object' || !isNum(v.value) || !units.includes(v.unit)) return `a dimension ({ value, unit: ${units.join(' | ')} })`
  if (v.value < 0 || v.value > 10000) return 'a dimension between 0 and 10000'
  return null
}

function colourValue(v) {
  if (!v || typeof v !== 'object' || v.colorSpace !== 'srgb' || typeof v.hex !== 'string' || !hexRe.test(v.hex)) return 'an sRGB colour with a #rrggbb hex'
  if (!Array.isArray(v.components) || v.components.length !== 3 || !v.components.every((n) => isNum(n) && n >= 0 && n <= 1))
    return 'an sRGB colour with 3 components between 0 and 1'
  const fromHex = [1, 3, 5].map((i) => parseInt(v.hex.slice(i, i + 2), 16) / 255)
  if (fromHex.some((x, i) => Math.abs(x - v.components[i]) > 0.002)) return `an sRGB colour whose components match its hex ${v.hex}`
  if (v.alpha !== undefined && !(isNum(v.alpha) && v.alpha >= 0 && v.alpha <= 1)) return 'an alpha between 0 and 1'
  for (const k of Object.keys(v)) if (!['colorSpace', 'components', 'hex', 'alpha'].includes(k)) return `an sRGB colour without the field "${k}"`
  return null
}

/** Checks one value of `type`. `refs` resolves a reference to its type; null when it is valid. */
function checkValue(type, v, ctx) {
  if (typeof v === 'string' && refRe.test(v)) {
    const target = v.match(refRe)[1]
    const t = ctx.types.get(target)
    if (!t) return `a reference to a token that exists (${v} does not)`
    if (type && t !== type) return `a reference to a ${type} token (${v} is ${t})`
    return null
  }
  switch (type) {
    case 'color':
      return colourValue(v)
    case 'dimension':
      return dimension(v)
    case 'duration':
      return dimension(v, ['ms', 's'])
    case 'number':
      return isNum(v) && v >= 0 && v <= 1000 ? null : 'a number between 0 and 1000'
    case 'fontWeight':
      return Number.isInteger(v) && v >= 1 && v <= 1000 ? null : 'a font weight between 1 and 1000'
    case 'fontFamily': {
      const list = typeof v === 'string' ? [v] : v
      if (!Array.isArray(list) || !list.length || list.length > 12 || !list.every((f) => typeof f === 'string' && familyRe.test(f)))
        return 'a list of font family names (letters, digits, spaces and hyphens)'
      return null
    }
    case 'cubicBezier':
      return Array.isArray(v) && v.length === 4 && v.every(isNum) && v[0] >= 0 && v[0] <= 1 && v[2] >= 0 && v[2] <= 1
        ? null
        : 'four numbers of a cubic Bézier curve'
    case 'typography':
      return composite(v, ctx, {
        fontFamily: 'fontFamily',
        fontSize: 'dimension',
        fontWeight: 'fontWeight',
        lineHeight: 'dimension',
        letterSpacing: 'dimension',
      })
    case 'transition':
      return composite(v, ctx, { duration: 'duration', delay: 'duration', timingFunction: 'cubicBezier' })
    case 'shadow': {
      const list = Array.isArray(v) ? v : [v]
      for (const s of list) {
        const e = composite(s, ctx, { color: 'color', offsetX: 'dimension', offsetY: 'dimension', blur: 'dimension', spread: 'dimension' }, ['inset'])
        if (e) return e
        if (s.inset !== undefined && typeof s.inset !== 'boolean') return 'a shadow whose inset is true or false'
      }
      return null
    }
    case undefined: {
      // A keyword token (icon line caps): the core lists the words it allows.
      if (typeof v !== 'string' || !keywordRe.test(v)) return 'a keyword (lower-case letters and hyphens)'
      if (ctx.allowed && !ctx.allowed.includes(v)) return `one of ${ctx.allowed.join(', ')}`
      return null
    }
    default:
      return `a value of a type that themes cannot set (${type})`
  }
}

function composite(v, ctx, fields, optional = []) {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return `an object with ${Object.keys(fields).join(', ')}`
  for (const k of Object.keys(v)) if (!(k in fields) && !optional.includes(k)) return `an object without the field "${k}"`
  for (const [k, t] of Object.entries(fields)) {
    if (!(k in v)) return `an object with the field "${k}"`
    const e = checkValue(t, v[k], ctx)
    if (e) return `${k}: ${e}`
  }
  return null
}

/**
 * The problems of one override tree against its core tree: tokens the core does not define, group
 * or type changes, and values that are not of the token's type. `scope` names the file in messages.
 */
export function checkOverride(core, override, scope, extraCore = []) {
  const problems = []
  const coreTokens = new Map(flatten(core).map((t) => [t.path, t]))
  // A reference may point at any token of the theme's scheme (base and colour together).
  const types = new Map([...flatten(core), ...extraCore.flatMap((c) => flatten(c))].map((t) => [t.path, t.type]))
  const walk = (node, prefix) => {
    for (const [k, v] of Object.entries(node)) {
      const p = prefix ? `${prefix}.${k}` : k
      if (k.startsWith('$')) {
        if (k === '$description' && typeof v === 'string' && v.length <= 500) continue
        if (k === '$schema' && !prefix) continue
        problems.push(`${scope}: ${p}: a theme may set only $value and $description`)
        continue
      }
      if (!v || typeof v !== 'object' || Array.isArray(v)) {
        problems.push(`${scope}: ${p}: not a token or a group`)
        continue
      }
      if (isToken(v)) {
        const c = coreTokens.get(p)
        if (!c) {
          problems.push(`${scope}: ${p}: the core defines no such token (a theme only gives new values to existing tokens)`)
          continue
        }
        for (const key of Object.keys(v))
          if (!['$value', '$description'].includes(key)) problems.push(`${scope}: ${p}: a theme may set only $value and $description (not ${key})`)
        const allowed = c.token.$extensions?.['org.ruum.allowed']
        const e = checkValue(c.type, v.$value, { types, allowed })
        if (e) problems.push(`${scope}: ${p}: must be ${e}`)
      } else walk(v, p)
    }
  }
  walk(override, '')
  return problems
}

// ---------------------------------------------------------------------------------------------
// SVG: a small closed set of shapes, no scripts, no styles, no links, no text.
// ---------------------------------------------------------------------------------------------

const svgElements = new Set(['svg', 'g', 'path', 'rect', 'circle', 'ellipse', 'line', 'polyline', 'polygon', 'title'])
const svgAttributes = new Set([
  'xmlns',
  'viewBox',
  'width',
  'height',
  'd',
  'x',
  'y',
  'rx',
  'ry',
  'cx',
  'cy',
  'r',
  'x1',
  'y1',
  'x2',
  'y2',
  'points',
  'fill',
  'fill-rule',
  'clip-rule',
  'fill-opacity',
  'stroke',
  'stroke-width',
  'stroke-linecap',
  'stroke-linejoin',
  'stroke-miterlimit',
  'stroke-opacity',
  'opacity',
  'transform',
])
const paintRe = /^(none|currentColor|#[0-9a-fA-F]{3}|#[0-9a-fA-F]{6}|var\(--color-[a-z0-9-]+(, *#[0-9a-fA-F]{6})?\))$/
const plainRe = /^[0-9a-zA-Z .,\-#()]*$/

/**
 * Parses a theme SVG into a tree of { tag, attrs, children }, or returns { problems }. The parser
 * accepts only plain elements and attributes of the lists above: no text, no CDATA, no entities,
 * no processing instructions other than the XML declaration, no comments.
 */
export function parseSvg(text) {
  const problems = []
  let s = text.replace(/^﻿/, '').replace(/^<\?xml[^?]*\?>/, '').trim()
  const root = { tag: '#root', attrs: {}, children: [] }
  const stack = [root]
  const tagRe = /^<(\/?)([a-zA-Z][a-zA-Z0-9]*)((?:\s+[a-zA-Z][a-zA-Z0-9:-]*\s*=\s*"[^"<>&]*")*)\s*(\/?)>/
  while (s.length) {
    const ws = s.match(/^\s+/)
    if (ws) {
      s = s.slice(ws[0].length)
      continue
    }
    if (s.startsWith('<title>')) {
      // A title is the only text allowed; it is dropped (the page gives the accessible name).
      const end = s.indexOf('</title>')
      const inner = end < 0 ? '' : s.slice(7, end)
      if (end < 0 || /[<&]/.test(inner)) return { problems: ['the title holds markup or is not closed'] }
      s = s.slice(end + 8)
      continue
    }
    const m = s.match(tagRe)
    if (!m) return { problems: [...problems, `unexpected content near "${s.slice(0, 40)}" (only plain shape elements are allowed)`] }
    const [all, close, tag, attrText, selfClose] = m
    s = s.slice(all.length)
    if (!svgElements.has(tag)) {
      problems.push(`the element <${tag}> is not allowed`)
      if (!close && !selfClose) stack.push({ tag, attrs: {}, children: [] })
      else if (close && stack.at(-1).tag === tag) stack.pop()
      continue
    }
    if (close) {
      if (stack.at(-1).tag !== tag) return { problems: [`</${tag}> does not close <${stack.at(-1).tag}>`] }
      stack.pop()
      continue
    }
    const attrs = {}
    for (const a of attrText.matchAll(/([a-zA-Z][a-zA-Z0-9:-]*)\s*=\s*"([^"]*)"/g)) {
      const [, name, value] = a
      if (!svgAttributes.has(name)) problems.push(`<${tag}>: the attribute ${name} is not allowed`)
      else if (name === 'xmlns' && value !== 'http://www.w3.org/2000/svg') problems.push(`<${tag}>: xmlns must be the SVG namespace`)
      else if ((name === 'fill' || name === 'stroke') && !paintRe.test(value))
        problems.push(`<${tag}>: ${name}="${value}" must be none, currentColor, a hex colour or var(--color-...)`)
      else if (name !== 'xmlns' && name !== 'fill' && name !== 'stroke' && !plainRe.test(value))
        problems.push(`<${tag}>: ${name} holds characters that are not allowed`)
      else attrs[name] = value
    }
    const node = { tag, attrs, children: [] }
    stack.at(-1).children.push(node)
    if (!selfClose) stack.push(node)
  }
  if (stack.length !== 1) problems.push(`<${stack.at(-1).tag}> is not closed`)
  const [svg, ...rest] = root.children
  if (!svg || svg.tag !== 'svg' || rest.length) problems.push('the file must hold exactly one <svg> element')
  else if (!/^-?[\d.]+ -?[\d.]+ [\d.]+ [\d.]+$/.test(svg.attrs.viewBox ?? '')) problems.push('<svg> needs a viewBox')
  return problems.length ? { problems } : { tree: svg }
}

// ---------------------------------------------------------------------------------------------
// A package
// ---------------------------------------------------------------------------------------------

/** The manifest and token overrides of a theme folder (no checks; check-theme.mjs checks). */
export function loadTheme(dir) {
  const manifest = readJson(path.join(dir, 'theme.json'))
  const overrides = {}
  for (const k of Object.keys(coreFiles)) {
    const f = manifest.tokens?.[k]
    overrides[k] = f ? readJson(path.join(dir, f)) : {}
  }
  return { dir, manifest, overrides }
}

/** The effective token trees of a theme: { base, light, dark } of core values with its overrides. */
export function effectiveTokens(theme, core = loadCore()) {
  return Object.fromEntries(Object.keys(coreFiles).map((k) => [k, merge(core[k], theme?.overrides[k])]))
}

/** The folder of a built-in theme. */
export const themeDir = (id) => path.join(themesDir, id)

/** The PNG width and height, from the IHDR chunk. */
export function pngSize(buf) {
  const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
  if (buf.length < 24 || !buf.subarray(0, 8).equals(sig) || buf.toString('latin1', 12, 16) !== 'IHDR') return null
  return { width: buf.readUInt32BE(16), height: buf.readUInt32BE(20) }
}

/** True when the buffer starts like a WOFF2 font. */
export const isWoff2 = (buf) => buf.length > 48 && buf.toString('latin1', 0, 4) === 'wOF2'
