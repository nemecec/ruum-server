// Validates theme packages (design 12.6) and refuses a failing one with the reasons listed:
//
// 1. theme.json against src/design/themes/theme.schema.json.
// 2. Files: only the types of a theme (JSON, SVG, PNG, WOFF2, licence text), each under its size
//    limit, the content matches the type (PNG and WOFF2 signatures, PNG sizes as declared), no
//    file that the manifest does not name, no links, nothing outside the folder.
// 3. Token overrides: only new values for tokens the core defines, each of the token's type. No
//    new token, no CSS, no free text.
// 4. SVGs: plain shapes only (no script, style, text, links or event attributes).
// 5. Contrast: every text, control and tag pair (theme-lib.mjs `pairs`) in light and dark, with
//    the theme over the core tokens. A failing pair refuses the theme; the pairs are listed.
//
//   node scripts/check-theme.mjs                 every theme in src/design/themes/
//   node scripts/check-theme.mjs <folder> ...    the given theme folders
//   --verbose                                    also list the passing pairs
//
// Exit code 1 when a theme fails. Part of `npm run lint`.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import Ajv2020 from 'ajv/dist/2020.js'
import {
  checkOverride,
  contrast,
  coreFiles,
  effectiveTokens,
  fileTypes,
  fontSubsets,
  isWoff2,
  limits,
  loadCore,
  parseSvg,
  pngSize,
  themesDir,
} from './theme-lib.mjs'

const schema = JSON.parse(fs.readFileSync(path.join(themesDir, 'theme.schema.json'), 'utf8'))
const validate = new Ajv2020({ allErrors: true, strict: false }).compile(schema)

/** Every file under `dir`, relative, with `/` separators. Links are reported, not followed. */
function listFiles(dir, problems) {
  const out = []
  const walk = (d) => {
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name)
      const rel = path.relative(dir, p).split(path.sep).join('/')
      if (e.isSymbolicLink()) problems.push(`${rel}: links are not allowed`)
      else if (e.isDirectory()) walk(p)
      else if (e.isFile()) out.push(rel)
      else problems.push(`${rel}: not a plain file`)
    }
  }
  walk(dir)
  return out
}

/**
 * Checks one theme folder. Returns { id, problems: [string], pairs: [result] }; the theme is
 * accepted when `problems` is empty.
 */
export function checkTheme(dir, core = loadCore()) {
  const problems = []
  const fail = (m) => problems.push(m)
  const manifestFile = path.join(dir, 'theme.json')
  if (!fs.existsSync(manifestFile)) return { id: path.basename(dir), problems: ['theme.json is missing'], pairs: [] }

  // 2a. The files: types, sizes, signatures.
  const files = listFiles(dir, problems)
  let total = 0
  if (files.length > limits.files) fail(`${files.length} files: a theme holds at most ${limits.files}`)
  const bytes = new Map()
  for (const f of files) {
    const buf = fs.readFileSync(path.join(dir, f))
    bytes.set(f, buf)
    total += buf.length
    const type = fileTypes[path.extname(f).toLowerCase()]
    if (!type) {
      fail(`${f}: the file type is not allowed (only ${Object.keys(fileTypes).join(', ')})`)
      continue
    }
    const limit = f === 'theme.json' ? limits.manifest : type === 'json' ? limits.tokens : limits[type]
    if (buf.length > limit) fail(`${f}: ${buf.length} bytes, over the limit of ${limit} for ${type} files`)
    if (type === 'png' && !pngSize(buf)) fail(`${f}: not a PNG file`)
    if (type === 'woff2' && !isWoff2(buf)) fail(`${f}: not a WOFF2 font`)
    if (type === 'text' || type === 'json' || type === 'svg') {
      const text = buf.toString('utf8')
      if (text.includes('�') || text.includes('\u0000')) fail(`${f}: not UTF-8 text`)
    }
  }
  if (total > limits.total) fail(`the theme has ${total} bytes, over the limit of ${limits.total}`)

  // 1. The manifest.
  let manifest
  try {
    manifest = JSON.parse(bytes.get('theme.json')?.toString('utf8') ?? '')
  } catch (e) {
    return { id: path.basename(dir), problems: [...problems, `theme.json: not JSON (${e.message})`], pairs: [] }
  }
  const id = typeof manifest.id === 'string' ? manifest.id : path.basename(dir)
  if (!validate(manifest)) {
    for (const e of validate.errors) fail(`theme.json${e.instancePath || ''}: ${e.message}${e.params?.additionalProperty ? ` (${e.params.additionalProperty})` : ''}`)
    return { id, problems, pairs: [] }
  }

  // 2b. Every file is named by the manifest (no hidden extras), and every named file exists.
  const named = new Set(['theme.json'])
  const need = (f, what) => {
    named.add(f)
    if (!bytes.has(f)) fail(`${f}: missing (${what})`)
  }
  for (const [k, f] of Object.entries(manifest.tokens)) need(f, `tokens.${k}`)
  if (manifest.assets.wordmark) need(manifest.assets.wordmark, 'assets.wordmark')
  need(manifest.assets.favicon, 'assets.favicon')
  for (const icon of manifest.assets.appIcons ?? []) {
    need(icon.src, 'assets.appIcons')
    const size = bytes.has(icon.src) && pngSize(bytes.get(icon.src))
    if (size && (size.width !== icon.size || size.height !== icon.size))
      fail(`${icon.src}: ${size.width} x ${size.height} px, but the manifest says ${icon.size} x ${icon.size}`)
  }
  const faces = new Set()
  for (const font of manifest.fonts ?? []) {
    need(font.src, 'fonts')
    const key = `${font.family}/${font.weight}/${font.style}/${font.subset}`
    if (faces.has(key)) fail(`fonts: ${key} is declared twice`)
    faces.add(key)
    if (!fontSubsets[font.subset]) fail(`fonts: unknown subset ${font.subset}`)
  }
  if (manifest.fontLicense) need(manifest.fontLicense, 'fontLicense')
  for (const f of files) if (!named.has(f)) fail(`${f}: the manifest does not name this file`)

  // 3. Token overrides.
  const overrides = {}
  for (const k of Object.keys(coreFiles)) {
    const f = manifest.tokens[k]
    if (!f || !bytes.has(f)) {
      overrides[k] = {}
      continue
    }
    try {
      overrides[k] = JSON.parse(bytes.get(f).toString('utf8'))
    } catch (e) {
      fail(`${f}: not JSON (${e.message})`)
      overrides[k] = {}
      continue
    }
    if (typeof overrides[k] !== 'object' || Array.isArray(overrides[k]) || !overrides[k]) {
      fail(`${f}: not a token tree`)
      overrides[k] = {}
      continue
    }
    // A colour file may refer to base tokens and the other way round, as the core files do.
    const others = k === 'base' ? [core.light] : [core.base]
    problems.push(...checkOverride(core[k], overrides[k], f, others))
  }

  // 4. SVGs.
  for (const f of files.filter((f) => f.endsWith('.svg'))) {
    const { problems: svgProblems } = parseSvg(bytes.get(f).toString('utf8'))
    for (const p of svgProblems ?? []) fail(`${f}: ${p}`)
  }
  if (manifest.assets.favicon && bytes.has(manifest.assets.favicon) && /var\(/.test(bytes.get(manifest.assets.favicon).toString('utf8')))
    fail(`${manifest.assets.favicon}: the favicon cannot read the page's custom properties; use fixed colours`)

  // 5. Contrast, with the theme over the core.
  const pairs = contrast(effectiveTokens({ overrides }, core))
  for (const p of pairs.filter((p) => !p.ok)) {
    fail(
      p.missing
        ? `contrast ${p.scheme}: ${p.fg} on ${p.bg} has no opaque colour (${p.what})`
        : `contrast ${p.scheme}: ${p.fg} on ${p.bg} is ${p.r.toFixed(2)}:1, needs ${p.min}:1 (${p.what})`,
    )
  }
  return { id, problems, pairs }
}

/** The theme folders under src/design/themes/ (each folder with a theme.json). */
export function builtInThemes() {
  return fs
    .readdirSync(themesDir, { withFileTypes: true })
    .filter((e) => e.isDirectory() && fs.existsSync(path.join(themesDir, e.name, 'theme.json')))
    .map((e) => path.join(themesDir, e.name))
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2)
  const verbose = args.includes('--verbose')
  const dirs = args.filter((a) => !a.startsWith('--'))
  const targets = dirs.length ? dirs.map((d) => path.resolve(d)) : builtInThemes()
  let failed = 0
  const core = loadCore()
  for (const dir of targets) {
    const { id, problems, pairs } = checkTheme(dir, core)
    const lowest = Object.fromEntries(
      ['light', 'dark'].map((s) => {
        const text = pairs.filter((p) => p.scheme === s && p.min === 4.5 && !p.missing).sort((a, b) => a.r - b.r)[0]
        return [s, text ? `${text.fg} on ${text.bg} ${text.r.toFixed(2)}:1` : '-']
      }),
    )
    if (verbose)
      for (const p of pairs) console.log(`  ${p.ok ? 'pass' : 'FAIL'}  ${p.scheme.padEnd(5)} ${p.r.toFixed(2).padStart(5)} >= ${p.min}  ${p.fg} on ${p.bg} (${p.what})`)
    if (problems.length) {
      failed++
      console.error(`theme ${id}: REFUSED, ${problems.length} problem(s):`)
      for (const p of problems) console.error(`  - ${p}`)
    } else {
      console.log(`theme ${id}: ok (${pairs.length} contrast pairs pass; lowest text pair: light ${lowest.light}, dark ${lowest.dark})`)
    }
  }
  process.exit(failed ? 1 : 0)
}
