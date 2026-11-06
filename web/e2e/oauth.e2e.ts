// The OAuth consent screen with the real server (journeys J5): an app registers itself, Ann opens
// the authorize URL, the consent screen names the app and where it goes back to, and the agent that
// the consent creates carries the app's name. Axe checks the screen too.
import crypto from 'node:crypto'
import AxeBuilder from '@axe-core/playwright'
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { api, tenantOf, uniqueName } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

const REDIRECT = 'http://127.0.0.1:33418/callback'

test('the consent screen names the app, and the new agent gets its name', async ({ page }) => {
  const appName = uniqueName('Claude Desktop', readRun().run)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  const tenant = tenantOf(page)

  // Dynamic client registration (RFC 7591), as an MCP client does it.
  const reg = await api<{ client_id: string }>(page, 'POST', '/v1/oauth/register', {
    body: { client_name: appName, redirect_uris: [REDIRECT], token_endpoint_auth_method: 'none' },
  })
  expect(reg.status).toBe(201)

  const verifier = crypto.randomBytes(32).toString('base64url')
  const challenge = crypto.createHash('sha256').update(verifier).digest('base64url')
  const q = new URLSearchParams({
    response_type: 'code',
    client_id: reg.body.client_id,
    redirect_uri: REDIRECT,
    code_challenge: challenge,
    code_challenge_method: 'S256',
    state: 'e2e-state',
  })
  // The app's own callback is not running: answer it here and keep the URL.
  let callback: URL | null = null
  await page.route('http://127.0.0.1:33418/**', async (route) => {
    callback = new URL(route.request().url())
    await route.fulfill({ status: 200, contentType: 'text/html', body: '<!doctype html><title>Callback</title><h1>Callback</h1>' })
  })

  await page.goto(`/v1/oauth/authorize?${q}`)
  await page.waitForURL(/\/oauth\/consent\?request=/)
  await expect(page.getByRole('heading', { name: `${appName} wants to connect to Ruum`, level: 1 })).toBeVisible()
  await expect(page.getByRole('note')).toContainText("Ruum can't check the name or address of this app.")
  await expect(page.getByText('It will go back to 127.0.0.1:33418.')).toBeVisible()
  await expect(page.getByLabel('Agent name')).toHaveAttribute('placeholder', appName)

  // Axe on the consent screen.
  const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']).analyze()
  const bad = result.violations.filter((v) => v.impact === 'serious' || v.impact === 'critical').map((v) => `${v.id}: ${v.help}`)
  expect(bad, 'axe on the consent screen').toEqual([])

  await page.getByRole('radio', { name: /^Everywhere you have access in / }).check()
  await page.getByRole('button', { name: 'Allow', exact: true }).click()
  await expect.poll(() => callback?.searchParams.get('state') ?? null).toBe('e2e-state')
  expect(callback!.searchParams.get('code')).toBeTruthy()

  // The agent carries the app's name, so its changes say "<app> for Ann", never "An app".
  // (The page is on the app's callback now; the API answers on the UI's origin.)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  const agents = await api<{ items: { name: string }[] }>(page, 'GET', '/v1/agents', { tenant })
  expect(agents.body.items.map((a) => a.name)).toContain(appName)
  expect(agents.body.items.map((a) => a.name)).not.toContain('An app')
})
