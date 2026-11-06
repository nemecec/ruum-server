#!/usr/bin/env node
// A small persistent browser driver for agents that act like users from the shell.
//
//   node scripts/ux/drive.mjs start --port 9333 --viewport 1280x800 --scheme light
//   node scripts/ux/drive.mjs goto http://localhost:5182/
//   node scripts/ux/drive.mjs snapshot
//   node scripts/ux/drive.mjs click "button:New folder"
//   node scripts/ux/drive.mjs stop
//
// `start` runs a background process with its own Chromium and its own profile (cookies, local
// storage). The other commands talk to it over HTTP on 127.0.0.1:<port>. Each port is one session.
// See scripts/ux/README.md.

import { spawn } from 'node:child_process'
import fs from 'node:fs'
import http from 'node:http'
import { createRequire } from 'node:module'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = path.dirname(fileURLToPath(import.meta.url))
const REPO = path.resolve(HERE, '../..')
const DEFAULT_PORT = Number(process.env.RUUM_DRIVE_PORT ?? 9333)
const BASE_DIR = process.env.RUUM_DRIVE_DIR ?? path.join(os.tmpdir(), 'ruum-drive')

const USAGE = `usage: node scripts/ux/drive.mjs <command> [args] [--port N]

session:
  start [--port 9333] [--viewport 1280x800|390x844] [--scheme light|dark] [--headed] [--profile DIR]
                            a new empty profile each time, or DIR to keep cookies across sessions
  stop                      close the browser (the profile folder stays; see README)
  status                    the session's settings and the current page

actions (a target is a ref from 'snapshot' like e12, 'role:name' like 'button:Upload',
or visible text; add --nth N when a target matches more than one element):
  goto <url>                open a URL (relative URLs use the current origin)
  click <target>
  fill <label|target> <text>
  select <label|target> <option>
  press <key>               for example Enter, Escape, Tab, Control+A
  upload <label|target> <file...>
  wait <text|ms>            wait until the text is visible, or for ms milliseconds
  back                      go back in history

reading:
  snapshot [--full]         accessibility tree with refs (first 150 lines unless --full)
  text                      the visible text of the page (first 4000 characters)
  screenshot <file> [--full-page]
  console [--all]           console errors, page errors and failed requests since the last call
  tabs | tab <n>            list the open tabs, or switch to one
`

// ------------------------------------------------------------------ argument parsing

function parseArgs(argv) {
  const flags = {}
  const rest = []
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a.startsWith('--')) {
      const key = a.slice(2)
      const next = argv[i + 1]
      if (next !== undefined && !next.startsWith('--') && VALUE_FLAGS.has(key)) {
        flags[key] = next
        i++
      } else {
        flags[key] = true
      }
    } else {
      rest.push(a)
    }
  }
  return { flags, rest }
}

const VALUE_FLAGS = new Set(['port', 'viewport', 'scheme', 'nth', 'timeout', 'profile'])

function sessionDir(port) {
  return path.join(BASE_DIR, String(port))
}

// ------------------------------------------------------------------ client side

function request(port, body, timeoutMs = 120_000) {
  return new Promise((resolve, reject) => {
    const data = JSON.stringify(body)
    const req = http.request(
      { host: '127.0.0.1', port, path: '/cmd', method: 'POST', headers: { 'content-type': 'application/json' }, timeout: timeoutMs },
      (res) => {
        let text = ''
        res.setEncoding('utf8')
        res.on('data', (c) => (text += c))
        res.on('end', () => {
          try {
            resolve(JSON.parse(text))
          } catch {
            reject(new Error(`bad answer from the driver: ${text.slice(0, 200)}`))
          }
        })
      },
    )
    req.on('timeout', () => req.destroy(new Error('the driver did not answer in time')))
    req.on('error', reject)
    req.end(data)
  })
}

async function alive(port) {
  try {
    const res = await request(port, { cmd: 'ping' }, 2000)
    return res.ok
  } catch {
    return false
  }
}

async function start(flags) {
  const port = Number(flags.port ?? DEFAULT_PORT)
  if (await alive(port)) {
    console.error(`a session already runs on port ${port}; stop it first or use another --port`)
    process.exit(1)
  }
  const viewport = String(flags.viewport ?? '1280x800')
  if (!/^\d+x\d+$/.test(viewport)) {
    console.error('--viewport must look like 1280x800')
    process.exit(2)
  }
  const scheme = String(flags.scheme ?? 'light')
  if (!['light', 'dark'].includes(scheme)) {
    console.error('--scheme must be light or dark')
    process.exit(2)
  }
  const dir = sessionDir(port)
  fs.mkdirSync(dir, { recursive: true })
  const log = fs.openSync(path.join(dir, 'driver.log'), 'a')
  const args = [fileURLToPath(import.meta.url), '__serve', '--port', String(port), '--viewport', viewport, '--scheme', scheme]
  if (flags.headed) args.push('--headed')
  // A new profile folder per session, so sessions never share cookies or storage.
  const profile = flags.profile ? path.resolve(String(flags.profile)) : path.join(dir, `profile-${Date.now()}`)
  args.push('--profile', profile)
  const child = spawn(process.execPath, args, { detached: true, stdio: ['ignore', log, log] })
  child.unref()
  for (let i = 0; i < 100; i++) {
    await new Promise((r) => setTimeout(r, 200))
    if (await alive(port)) {
      console.log(`session on port ${port}: Chromium ${flags.headed ? 'headed' : 'headless'}, viewport ${viewport}, ${scheme} scheme`)
      console.log(`profile: ${profile}`)
      console.log(`log: ${path.join(dir, 'driver.log')}`)
      return
    }
    if (child.exitCode !== null) break
  }
  console.error(`the driver did not start; see ${path.join(dir, 'driver.log')}`)
  process.exit(1)
}

async function client(cmd, rest, flags) {
  const port = Number(flags.port ?? DEFAULT_PORT)
  let res
  try {
    res = await request(port, { cmd, args: rest, flags }, Number(flags.timeout ?? 120_000) + 5000)
  } catch (e) {
    if (cmd === 'stop') {
      console.log(`no session on port ${port}`)
      return
    }
    console.error(`no session on port ${port} (${e.message}); run: node scripts/ux/drive.mjs start --port ${port}`)
    process.exit(1)
  }
  if (res.output) console.log(res.output)
  if (!res.ok) {
    console.error(`error: ${res.error}`)
    process.exit(1)
  }
}

// ------------------------------------------------------------------ server side

function loadPlaywright() {
  const candidates = [process.env.RUUM_PLAYWRIGHT_FROM, path.join(REPO, 'web/package.json'), path.join(HERE, 'package.json')].filter(Boolean)
  for (const from of candidates) {
    try {
      return createRequire(from)('playwright-core')
    } catch {
      /* next */
    }
  }
  throw new Error('playwright-core not found: run `npm ci` in web/ (it is a dev dependency there)')
}

const ROLES = new Set([
  'alert', 'alertdialog', 'application', 'article', 'banner', 'blockquote', 'button', 'caption', 'cell', 'checkbox',
  'code', 'columnheader', 'combobox', 'complementary', 'contentinfo', 'definition', 'deletion', 'dialog', 'directory',
  'document', 'emphasis', 'feed', 'figure', 'form', 'generic', 'grid', 'gridcell', 'group', 'heading', 'img',
  'insertion', 'link', 'list', 'listbox', 'listitem', 'log', 'main', 'marquee', 'math', 'meter', 'menu', 'menubar',
  'menuitem', 'menuitemcheckbox', 'menuitemradio', 'navigation', 'none', 'note', 'option', 'paragraph', 'presentation',
  'progressbar', 'radio', 'radiogroup', 'region', 'row', 'rowgroup', 'rowheader', 'scrollbar', 'search', 'searchbox',
  'separator', 'slider', 'spinbutton', 'status', 'strong', 'subscript', 'superscript', 'switch', 'tab', 'table',
  'tablist', 'tabpanel', 'term', 'textbox', 'time', 'timer', 'toolbar', 'tooltip', 'tree', 'treegrid', 'treeitem',
])

async function serve(flags) {
  const { chromium } = loadPlaywright()
  const port = Number(flags.port)
  const [width, height] = String(flags.viewport).split('x').map(Number)
  const dir = sessionDir(port)
  const profile = String(flags.profile)
  const context = await chromium.launchPersistentContext(profile, {
    headless: !flags.headed,
    viewport: { width, height },
    colorScheme: flags.scheme,
    acceptDownloads: true,
    isMobile: false,
    hasTouch: width < 768,
    deviceScaleFactor: width < 768 ? 2 : 1,
  })
  context.setDefaultTimeout(10_000)
  const events = [] // console errors, page errors, failed requests
  let active = context.pages()[0] ?? (await context.newPage())

  function watch(page) {
    page.on('console', (m) => {
      if (m.type() === 'error' || m.type() === 'warning') events.push(`[console.${m.type()}] ${m.text()} (${page.url()})`)
    })
    page.on('pageerror', (e) => events.push(`[pageerror] ${e.message} (${page.url()})`))
    page.on('requestfailed', (r) => events.push(`[requestfailed] ${r.method()} ${r.url()}: ${r.failure()?.errorText}`))
    page.on('response', (r) => {
      if (r.status() >= 500) events.push(`[http ${r.status()}] ${r.request().method()} ${r.url()}`)
    })
    page.on('download', async (d) => {
      const file = path.join(dir, 'downloads', d.suggestedFilename())
      fs.mkdirSync(path.dirname(file), { recursive: true })
      try {
        await d.saveAs(file)
        events.push(`[download] saved ${file}`)
      } catch (e) {
        events.push(`[download] failed: ${e.message}`)
      }
    })
  }
  context.pages().forEach(watch)
  context.on('page', (p) => {
    watch(p)
    active = p // a new tab (preview, popup) becomes the active one, as for a person
  })

  function pageNow() {
    if (active.isClosed()) active = context.pages().at(-1)
    return active
  }

  /** Resolves a target: a ref (e12), role:name, or text. */
  async function resolve(target, opts = {}) {
    const page = pageNow()
    const nth = opts.nth !== undefined ? Number(opts.nth) : undefined
    const tries = []
    if (/^(f\d+)?e\d+$/.test(target)) {
      tries.push(page.getByRef(target))
    } else {
      const m = /^([a-z]+):(.*)$/s.exec(target)
      if (m && ROLES.has(m[1])) {
        const [, role, name] = m
        tries.push(page.getByRole(role, name ? { name, exact: true } : {}))
        if (name) tries.push(page.getByRole(role, { name }))
      } else if (opts.field) {
        tries.push(page.getByLabel(target, { exact: true }), page.getByLabel(target), page.getByPlaceholder(target))
        tries.push(page.getByRole('textbox', { name: target }), page.getByRole('combobox', { name: target }))
      } else {
        tries.push(page.getByText(target, { exact: true }), page.getByRole('button', { name: target }), page.getByRole('link', { name: target }))
        tries.push(page.getByText(target))
      }
    }
    // The page may still be loading: look again for a few seconds before giving up.
    const deadline = Date.now() + Number(opts.timeout ?? 5000)
    for (;;) {
      for (const loc of tries) {
        const all = loc.filter({ visible: true })
        const count = await all.count()
        if (count === 0) continue
        if (count > 1 && nth === undefined) {
          const names = []
          for (let i = 0; i < Math.min(count, 5); i++) {
            names.push((await all.nth(i).evaluate((el) => (el.innerText || el.getAttribute('aria-label') || el.value || el.tagName).trim().slice(0, 60))).replace(/\s+/g, ' '))
          }
          throw new Error(`"${target}" matches ${count} elements (${names.map((n) => JSON.stringify(n)).join(', ')}); add --nth N (0-based) or use a ref from snapshot`)
        }
        return all.nth(nth ?? 0)
      }
      if (Date.now() > deadline) break
      await page.waitForTimeout(250)
    }
    throw new Error(`nothing matches "${target}"; run snapshot to see the refs and names`)
  }

  async function describe(loc) {
    return loc
      .evaluate((el) => {
        const role = el.getAttribute('role') || el.tagName.toLowerCase()
        const name = el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('name') || ''
        return `${role} "${name.trim().replace(/\s+/g, ' ').slice(0, 60)}"`
      })
      .catch(() => 'element')
  }

  /** Waits a moment for what an action starts: a navigation, a redirect chain, network calls. */
  async function settle(page) {
    await page.waitForEvent('framenavigated', { timeout: 700 }).catch(() => {})
    await page.waitForLoadState('domcontentloaded', { timeout: 10_000 }).catch(() => {})
    await page.waitForLoadState('networkidle', { timeout: 3000 }).catch(() => {})
  }

  const commands = {
    ping: async () => 'pong',
    status: async () => {
      const page = pageNow()
      return `port ${port}, viewport ${width}x${height}, ${flags.scheme} scheme, ${context.pages().length} tab(s)\nprofile: ${profile}\nurl: ${page.url()}\ntitle: ${await page.title()}`
    },
    goto: async ([url]) => {
      if (!url) throw new Error('goto <url>')
      const page = pageNow()
      const target = /^[a-z]+:/i.test(url) ? url : new URL(url, page.url() === 'about:blank' ? undefined : page.url()).toString()
      const res = await page.goto(target, { waitUntil: 'domcontentloaded', timeout: 30_000 })
      await page.waitForLoadState('networkidle', { timeout: 5000 }).catch(() => {})
      return `${res?.status() ?? ''} ${page.url()}\ntitle: ${await page.title()}`
    },
    back: async () => {
      const page = pageNow()
      await page.goBack({ waitUntil: 'domcontentloaded' })
      return page.url()
    },
    click: async ([target], f) => {
      if (!target) throw new Error('click <target>')
      const loc = await resolve(target, f)
      const what = await describe(loc)
      await loc.click()
      const page = pageNow()
      await settle(page)
      return `clicked ${what}\nurl: ${page.url()}`
    },
    fill: async ([target, ...text], f) => {
      if (!target || text.length === 0) throw new Error('fill <label> <text>')
      const loc = await resolve(target, { ...f, field: true })
      await loc.fill(text.join(' '))
      return `filled ${await describe(loc)}`
    },
    select: async ([target, ...option], f) => {
      if (!target || option.length === 0) throw new Error('select <label> <option>')
      const loc = await resolve(target, { ...f, field: true })
      const label = option.join(' ')
      const picked = await loc.selectOption({ label }).catch(() => loc.selectOption(label))
      await settle(pageNow())
      return `selected ${picked.join(', ')} in ${await describe(loc)}`
    },
    press: async ([key]) => {
      if (!key) throw new Error('press <key>')
      const page = pageNow()
      await page.keyboard.press(key)
      await settle(page)
      return `pressed ${key}\nurl: ${page.url()}`
    },
    upload: async ([target, ...files], f) => {
      if (!target || files.length === 0) throw new Error('upload <label> <file...>')
      const paths = files.map((p) => path.resolve(p))
      for (const p of paths) if (!fs.existsSync(p)) throw new Error(`no such file: ${p}`)
      const page = pageNow()
      // A file input (often hidden behind a button) takes the files directly.
      let input = null
      for (const loc of [page.getByLabel(target, { exact: true }), page.getByLabel(target)]) {
        const n = await loc.count()
        for (let i = 0; i < n; i++) {
          const el = loc.nth(i)
          if (await el.evaluate((e) => e instanceof HTMLInputElement && e.type === 'file')) {
            input = el
            break
          }
        }
        if (input) break
      }
      if (input) {
        await input.setInputFiles(paths)
      } else {
        // Otherwise the target is a button that opens the file chooser.
        const loc = await resolve(target, f)
        const [chooser] = await Promise.all([page.waitForEvent('filechooser', { timeout: 10_000 }), loc.click()])
        await chooser.setFiles(paths)
      }
      await settle(page)
      return `uploaded ${paths.map((p) => path.basename(p)).join(', ')}`
    },
    wait: async ([what], f) => {
      if (!what) throw new Error('wait <text|ms>')
      const page = pageNow()
      if (/^\d+$/.test(what)) {
        await page.waitForTimeout(Number(what))
        return `waited ${what} ms`
      }
      await page.getByText(what).filter({ visible: true }).first().waitFor({ state: 'visible', timeout: Number(f.timeout ?? 15_000) })
      return `visible: ${what}`
    },
    snapshot: async (_, f) => {
      const page = pageNow()
      const tree = await page.ariaSnapshot({ mode: 'ai' })
      const lines = tree.split('\n')
      const max = f.full ? Infinity : 150
      const shown = lines.slice(0, max).join('\n')
      const more = lines.length > max ? `\n... ${lines.length - max} more lines (snapshot --full)` : ''
      return `url: ${page.url()}\ntitle: ${await page.title()}\n${shown}${more}`
    },
    text: async () => {
      const page = pageNow()
      const t = await page.evaluate(() => document.body?.innerText ?? '')
      return t.length > 4000 ? `${t.slice(0, 4000)}\n... (${t.length} characters)` : t
    },
    screenshot: async ([file], f) => {
      if (!file) throw new Error('screenshot <file>')
      const out = path.resolve(file)
      fs.mkdirSync(path.dirname(out), { recursive: true })
      await pageNow().screenshot({ path: out, fullPage: Boolean(f['full-page']) })
      return `saved ${out}`
    },
    console: async (_, f) => {
      const list = f.all ? events : events.filter((e) => !e.startsWith('[console.warning]') && !e.startsWith('[download]'))
      const text = list.length ? list.join('\n') : 'no errors'
      events.length = 0
      return text
    },
    tabs: async () =>
      (await Promise.all(context.pages().map(async (p, i) => `${p === pageNow() ? '*' : ' '} ${i}: ${p.url()}  ${await p.title()}`))).join('\n'),
    tab: async ([n]) => {
      const p = context.pages()[Number(n)]
      if (!p) throw new Error(`no tab ${n}`)
      active = p
      await p.bringToFront()
      return `tab ${n}: ${p.url()}`
    },
  }

  const server = http.createServer((req, res) => {
    let body = ''
    req.on('data', (c) => (body += c))
    req.on('end', async () => {
      let answer
      try {
        const { cmd, args = [], flags: f = {} } = JSON.parse(body || '{}')
        if (cmd === 'stop') {
          res.end(JSON.stringify({ ok: true, output: `stopped the session on port ${port}` }))
          await shutdown()
          return
        }
        const fn = commands[cmd]
        if (!fn) throw new Error(`unknown command ${cmd}\n${USAGE}`)
        answer = { ok: true, output: await fn(args, f) }
      } catch (e) {
        answer = { ok: false, error: String(e?.message ?? e).split('\n').slice(0, 6).join('\n') }
      }
      res.setHeader('content-type', 'application/json')
      res.end(JSON.stringify(answer))
    })
  })

  let closing = false
  async function shutdown() {
    if (closing) return
    closing = true
    server.close()
    await context.close().catch(() => {})
    process.exit(0)
  }
  process.on('SIGTERM', shutdown)
  process.on('SIGINT', shutdown)
  // The browser went away on its own (crash, window closed in headed mode).
  context.on('close', () => {
    if (!closing) process.exit(0)
  })
  server.listen(port, '127.0.0.1')
}

// ------------------------------------------------------------------ main

// Flags may come before or after the command: `--port 9334 goto <url>` or `goto <url> --port 9334`.
const { flags, rest: words } = parseArgs(process.argv.slice(2))
const [cmd, ...rest] = words
if (!cmd || cmd === 'help' || flags.help || flags.h) {
  console.log(USAGE)
} else if (cmd === '__serve') {
  serve(flags).catch((e) => {
    console.error(e)
    process.exit(1)
  })
} else if (cmd === 'start') {
  await start(flags)
} else {
  await client(cmd, rest, flags)
}
