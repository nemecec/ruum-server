// The checks of theme packages (scripts/check-theme.mjs, design 12.6): the built-in themes pass,
// and each rule refuses what it is for, with the reason (and the failing pairs) listed.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { builtInThemes, checkTheme } from './check-theme.mjs'
import { DEFAULT_THEME, parseSvg, themeDir } from './theme-lib.mjs'

const temps = []
afterEach(() => {
  for (const d of temps.splice(0)) fs.rmSync(d, { recursive: true, force: true })
})

/** A copy of the Graphite package in a temporary folder, changed by `edit(dir)`. */
function variant(edit) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ruum-theme-'))
  temps.push(dir)
  fs.cpSync(themeDir(DEFAULT_THEME), dir, { recursive: true })
  edit(dir)
  return checkTheme(dir).problems
}
const readJson = (dir, f) => JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8'))
const writeJson = (dir, f, v) => fs.writeFileSync(path.join(dir, f), JSON.stringify(v, null, 2))
const editJson = (f, change) => (dir) => {
  const v = readJson(dir, f)
  change(v)
  writeJson(dir, f, v)
}
const colour = (hex) => ({ colorSpace: 'srgb', components: [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255), hex })

describe('check-theme', () => {
  it('accepts every built-in theme, with every pair checked in light and dark', () => {
    const dirs = builtInThemes()
    expect(dirs.map((d) => path.basename(d))).toContain('graphite')
    for (const dir of dirs) {
      const { problems, pairs } = checkTheme(dir)
      expect(problems, path.basename(dir)).toEqual([])
      expect(pairs.filter((p) => p.scheme === 'light').length).toBeGreaterThanOrEqual(42)
      expect(pairs.filter((p) => p.scheme === 'dark').length).toBe(pairs.filter((p) => p.scheme === 'light').length)
    }
  })

  it('refuses a theme whose text fails contrast and lists the failing pairs', () => {
    const problems = variant(
      editJson('tokens/color.light.tokens.json', (t) => {
        t.color['text-muted'].$value = colour('#a0a4aa')
      }),
    )
    expect(problems).toEqual(
      expect.arrayContaining([
        expect.stringMatching(/^contrast light: text-muted on canvas is 2\.\d\d:1, needs 4\.5:1/),
        expect.stringMatching(/^contrast light: text-muted on surface is /),
        expect.stringMatching(/^contrast light: text-muted on selected is /),
      ]),
    )
    expect(problems.every((p) => p.startsWith('contrast light: text-muted'))).toBe(true)
  })

  it('refuses a token that the core does not define (a theme adds no tokens)', () => {
    const problems = variant(
      editJson('tokens/color.dark.tokens.json', (t) => {
        t.color['glow'] = { $value: colour('#ff00ff') }
      }),
    )
    expect(problems).toContainEqual(expect.stringContaining('color.glow: the core defines no such token'))
  })

  it('refuses values that are not of the token type, and CSS smuggled into a value', () => {
    const problems = variant(
      editJson('tokens/base.tokens.json', (t) => {
        t.font.family.sans.$value = ['x;} body{background:url(https://evil.example/)']
        t.radius.sm.$value = { value: 2, unit: 'vw' }
        t.icon.linecap.$value = 'wavy'
        t.font.family.mono.$extensions = { hack: true }
      }),
    )
    expect(problems).toEqual(
      expect.arrayContaining([
        expect.stringContaining('font.family.sans: must be a list of font family names'),
        expect.stringContaining('radius.sm: must be a dimension'),
        expect.stringContaining('icon.linecap: must be one of round, square, butt'),
        expect.stringContaining('font.family.mono: a theme may set only $value and $description'),
      ]),
    )
  })

  it('refuses a colour whose components do not match its hex', () => {
    const problems = variant(
      editJson('tokens/color.light.tokens.json', (t) => {
        t.color.canvas.$value.components = [0, 0, 0]
      }),
    )
    expect(problems).toContainEqual(expect.stringContaining('color.canvas: must be an sRGB colour whose components match'))
  })

  it('refuses scripts, styles, links and event handlers in an SVG', () => {
    const svg = (body) => `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">${body}</svg>`
    expect(parseSvg(svg('<script>alert(1)</script>')).problems).toContainEqual('the element <script> is not allowed')
    expect(parseSvg(svg('<style>*{}</style>')).problems).toContainEqual('the element <style> is not allowed')
    expect(parseSvg(svg('<path d="M0 0" onload="x()"/>')).problems).toContainEqual('<path>: the attribute onload is not allowed')
    expect(parseSvg(svg('<path d="M0 0" fill="url(https://x.example/p)"/>')).problems?.[0]).toMatch(/fill=.* must be none, currentColor/)
    expect(parseSvg(svg('<use href="#a"/>')).problems).toContainEqual('the element <use> is not allowed')
    expect(parseSvg(svg('<text>hi</text>')).problems).toBeTruthy()
    expect(parseSvg('<!DOCTYPE svg><svg viewBox="0 0 1 1"/>').problems).toBeTruthy()
    expect(parseSvg(svg('<path d="M0 0" fill="var(--color-text, #16191d)"/>')).tree).toBeTruthy()

    const problems = variant((dir) => fs.writeFileSync(path.join(dir, 'assets/wordmark.svg'), svg('<script>alert(1)</script>')))
    expect(problems).toContainEqual('assets/wordmark.svg: the element <script> is not allowed')
  })

  it('refuses a favicon that reads custom properties (an image cannot see them)', () => {
    const problems = variant((dir) =>
      fs.writeFileSync(path.join(dir, 'assets/favicon.svg'), '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect width="4" height="4" fill="var(--color-primary)"/></svg>'),
    )
    expect(problems).toContainEqual(expect.stringContaining('the favicon cannot read the page'))
  })

  it('refuses files of other types, files the manifest does not name, and files over the limit', () => {
    const problems = variant((dir) => {
      fs.writeFileSync(path.join(dir, 'assets/run.js'), 'alert(1)')
      fs.writeFileSync(path.join(dir, 'assets/extra.svg'), '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>')
      fs.writeFileSync(path.join(dir, 'fonts/ibm-plex-sans-latin-400-normal.woff2'), Buffer.concat([Buffer.from('wOF2'), Buffer.alloc(300 * 1024)]))
    })
    expect(problems).toEqual(
      expect.arrayContaining([
        expect.stringContaining('assets/run.js: the file type is not allowed'),
        expect.stringContaining('assets/extra.svg: the manifest does not name this file'),
        expect.stringMatching(/fonts\/ibm-plex-sans-latin-400-normal\.woff2: \d+ bytes, over the limit/),
      ]),
    )
  })

  it('refuses content that does not match its type, and app icons of the wrong size', () => {
    const problems = variant((dir) => {
      fs.copyFileSync(path.join(dir, 'assets/icon-192.png'), path.join(dir, 'assets/icon-512.png'))
      fs.writeFileSync(path.join(dir, 'fonts/ibm-plex-mono-latin-400-normal.woff2'), 'not a font')
    })
    expect(problems).toEqual(
      expect.arrayContaining([
        'assets/icon-512.png: 192 x 192 px, but the manifest says 512 x 512',
        'fonts/ibm-plex-mono-latin-400-normal.woff2: not a WOFF2 font',
      ]),
    )
  })

  it('refuses a manifest that breaks the schema', () => {
    const problems = variant(
      editJson('theme.json', (m) => {
        m.script = 'boot.js'
        m.fonts[0].src = 'https://fonts.example/plex.woff2'
        delete m.license
      }),
    )
    expect(problems).toEqual(
      expect.arrayContaining([
        expect.stringContaining('must NOT have additional properties (script)'),
        expect.stringMatching(/theme\.json\/fonts\/0\/src: must match pattern/),
        expect.stringContaining("must have required property 'license'"),
      ]),
    )
  })

  it('refuses links that point out of the folder', () => {
    const problems = variant((dir) => fs.symlinkSync('/etc/hosts', path.join(dir, 'assets/hosts.txt')))
    expect(problems).toContainEqual('assets/hosts.txt: links are not allowed')
  })
})
