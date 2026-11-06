// Throwaway screenshot script for the UX review (docs/ux/gap-analysis.md).
//
// Run the web UI with the mock API first:
//   cd web && npx vite --mode mock --port 5183 --strictPort
// Then:
//   cd docs/ux/tools && npm install && node screenshots.mjs
//
// It uses an installed Playwright Chromium (CHROMIUM_PATH, or the default cache folder) and never
// downloads a browser. Pictures go to docs/ux/tools/shots/ (ignored by git).

import { chromium } from 'playwright-core'
import { existsSync, mkdirSync, readdirSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'

const BASE = process.env.RUUM_UX_URL ?? 'http://localhost:5183'
const OUT = new URL('./shots/', import.meta.url).pathname
mkdirSync(OUT, { recursive: true })

function chromiumPath() {
  if (process.env.CHROMIUM_PATH) return process.env.CHROMIUM_PATH
  const cache = join(homedir(), 'Library/Caches/ms-playwright')
  const dir = readdirSync(cache).filter((d) => /^chromium-\d+$/.test(d)).sort().pop()
  const p = join(cache, dir, 'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing')
  if (!existsSync(p)) throw new Error(`no Chromium at ${p}; set CHROMIUM_PATH`)
  return p
}

const browser = await chromium.launch({ executablePath: chromiumPath() })

async function session(name, { width, height, scheme }) {
  const ctx = await browser.newContext({ viewport: { width, height }, colorScheme: scheme })
  const page = await ctx.newPage()
  const shot = async (label) => {
    await page.waitForTimeout(500)
    await page.screenshot({ path: join(OUT, `${name}-${label}.png`), fullPage: true })
  }
  return { ctx, page, shot }
}

async function tour(name, opts) {
  const { ctx, page, shot } = await session(name, opts)
  await page.goto(`${BASE}/signin`)
  await shot('01-signin')
  await page.getByRole('link', { name: 'Continue with Google' }).click()
  await page.waitForURL(/\/t\/.+\/files/)
  await shot('02-files-root')
  const tenantUrl = page.url().replace(/\/files.*$/, '')

  await page.getByRole('button', { name: 'Details of notes.md' }).click()
  await shot('03-details')
  await page.getByRole('button', { name: 'Version history' }).click()
  await shot('04-versions')
  const compare = page.getByRole('button', { name: /^Compare version/ }).first()
  if (await compare.count()) {
    await compare.click()
    await shot('05-diff')
  }
  await page.getByRole('button', { name: 'Back to details' }).click()
  await page.getByRole('button', { name: 'Share', exact: true }).click()
  await shot('06-share')
  await page.keyboard.press('Escape')

  await page.getByRole('link', { name: 'Photos' }).click()
  await shot('07-empty-folder')

  for (const [path, label] of [
    ['shared', '08-shared'],
    ['trash', '09-trash'],
    ['search?q=engine', '10-search'],
    ['search?q=zzz', '11-search-empty'],
    ['agents', '12-agents'],
    ['account', '14-account'],
    ['admin/members', '15-admin-members'],
    ['admin/limits', '16-admin-limits'],
    ['admin/audit', '17-admin-audit'],
  ]) {
    await page.goto(`${tenantUrl}/${path}`)
    await shot(label)
  }
  await page.goto(`${tenantUrl}/agents`)
  await page.getByRole('link', { name: 'Claude Desktop' }).click()
  await shot('13-agent')
  await page.goto(`${BASE}/oauth/consent`)
  await shot('18-consent-missing')
  await page.goto(`${BASE}/nowhere`)
  await shot('19-not-found')
  await ctx.close()
}

await tour('desktop-light', { width: 1280, height: 800, scheme: 'light' })
await tour('desktop-dark', { width: 1280, height: 800, scheme: 'dark' })
await tour('phone-light', { width: 390, height: 844, scheme: 'light' })
await browser.close()
console.log(`screenshots in ${OUT}`)
