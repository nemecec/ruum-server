// Helpers for the browser tests: the real sign-in through Zitadel, API calls with the page's
// session, Mailpit, and test files.
import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { expect, type Page } from '@playwright/test'
import { env, type TestUser } from './env.ts'

/**
 * Signs in through the real OIDC flow: the sign-in page of the UI, `loginStart` on the server, the
 * Zitadel login UI (login name, password, skip the 2-factor prompt), `loginCallback` and back to the
 * UI. Ends on the files page of the user's home tenant.
 */
export async function signIn(page: Page, user: TestUser, returnTo = '/', opts: { shell?: boolean } = {}): Promise<void> {
  await page.goto(returnTo)
  await expect(page.getByRole('heading', { name: 'Sign in to Ruum' })).toBeVisible()
  // The buttons pass an `idp` hint that the server does not send to Zitadel yet (docs/STATUS.md),
  // so every button ends on the Zitadel login UI. The passkey button keeps that true later too.
  await page.getByRole('link', { name: 'Sign in with email' }).click()
  await page.waitForURL(/\/ui\/login\//)
  await page.getByLabel('Login Name').fill(user.loginName)
  await page.getByRole('button', { name: 'Next' }).click()
  await page.getByLabel('Password').fill(user.password)
  await page.getByRole('button', { name: 'Next' }).click()
  // Zitadel asks once to set up a second factor; the login policy allows skipping it.
  const skip = page.getByRole('button', { name: 'Skip' })
  const home = new URL(env.webUrl).origin
  await Promise.race([
    skip
      .waitFor({ state: 'visible', timeout: 15_000 })
      .then(() => skip.click())
      .catch(() => {}),
    page.waitForURL((u) => u.origin === home, { timeout: 15_000 }).catch(() => {}),
  ])
  await page.waitForURL((u) => u.origin === home && !u.pathname.startsWith('/signin'), { timeout: 15_000 })
  if (opts.shell !== false) await expect(page.getByRole('link', { name: user.displayName })).toBeVisible()
}

/** A toast (or a status message in a dialog) with this text; the newest one when there are several. */
export function toast(page: Page, text: string | RegExp) {
  return page.getByRole('status').filter({ hasText: text }).last()
}

/** The tenant ID in the current URL (`/t/<tenant>/...`). */
export function tenantOf(page: Page): string {
  const m = /\/t\/([0-9a-f-]{36})/.exec(page.url())
  if (!m) throw new Error(`no tenant in ${page.url()}`)
  return m[1]
}

/** Calls the API from the page, with its session cookie and CSRF header, as the UI does. */
export async function api<T = unknown>(
  page: Page,
  method: string,
  path: string,
  opts: { body?: unknown; tenant?: string } = {},
): Promise<{ status: number; body: T }> {
  return page.evaluate(
    async ({ method, path, body, tenant }) => {
      const csrf = document.cookie
        .split('; ')
        .find((c) => c.startsWith('ruum_csrf='))
        ?.slice('ruum_csrf='.length)
      const headers: Record<string, string> = { Accept: 'application/json' }
      if (csrf) headers['Ruum-Csrf'] = csrf
      if (tenant) headers['Ruum-Tenant'] = tenant
      if (body !== undefined) headers['Content-Type'] = 'application/json'
      if (method !== 'GET') headers['Idempotency-Key'] = crypto.randomUUID()
      const res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) })
      const text = await res.text()
      return { status: res.status, body: text ? JSON.parse(text) : null }
    },
    { method, path, body: opts.body, tenant: opts.tenant },
  )
}

interface MailSummary {
  ID: string
  Subject: string
  To: { Address: string }[]
}

/** Waits for a mail to `to` in Mailpit and returns its subject and plain text. */
export async function waitForMail(to: string, timeoutMs = 20_000, subject?: string): Promise<{ subject: string; text: string }> {
  const until = Date.now() + timeoutMs
  while (Date.now() < until) {
    const res = await fetch(`${env.mailpitUrl}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`)
    if (res.ok) {
      const list = (await res.json()) as { messages: MailSummary[] }
      // The newest mail to `to`, or the newest whose subject has `subject` in it.
      const m = list.messages?.find((x) => !subject || x.Subject.includes(subject))
      if (m) {
        const full = (await (await fetch(`${env.mailpitUrl}/api/v1/message/${m.ID}`)).json()) as { Text: string }
        return { subject: m.Subject, text: full.Text }
      }
    }
    await new Promise((r) => setTimeout(r, 500))
  }
  throw new Error(`no mail to ${to} in Mailpit after ${timeoutMs} ms`)
}

/** Writes a test file into the test's output folder and returns its path. */
export function testFile(dir: string, name: string, content: string | Buffer): string {
  fs.mkdirSync(dir, { recursive: true })
  const p = path.join(dir, name)
  fs.writeFileSync(p, content)
  return p
}

export function sha256(data: Buffer | string): string {
  return crypto.createHash('sha256').update(data).digest('hex')
}

/** The access token of an agent API key (token exchange), for calls as the agent. */
export async function agentToken(key: string): Promise<string> {
  const res = await fetch(`${env.serverUrl}/v1/oauth/token`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({
      grant_type: 'urn:ietf:params:oauth:grant-type:token-exchange',
      subject_token: key,
      subject_token_type: 'urn:ruum:params:oauth:token-type:api-key',
    }),
  })
  const body = (await res.json()) as { access_token?: string }
  if (!res.ok || !body.access_token) throw new Error(`token exchange: ${res.status} ${JSON.stringify(body)}`)
  return body.access_token
}

/** A name that is new on every call: `<prefix> <run>-<random>`. */
export function uniqueName(prefix: string, run: string): string {
  return `${prefix} ${run}-${crypto.randomBytes(2).toString('hex')}`
}

/** Invites `email` on Space settings > Invitations of the page's current space; returns the accept URL from Mailpit. */
export async function invite(page: Page, email: string, role: 'Guest' | 'Member' | 'Admin'): Promise<string> {
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' }).click()
  await page.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Invitations' }).click()
  const form = page.getByRole('form', { name: 'Invite people' })
  await form.getByLabel('Emails').fill(email)
  await form.getByLabel('Role').selectOption({ label: role })
  await form.getByLabel('Message (optional)').fill('Welcome to the e2e run.')
  await form.getByRole('button', { name: 'Send invitations' }).click()
  await expect(toast(page, `Invitation sent to ${email}.`)).toBeVisible()
  await expect(page.getByRole('listitem').filter({ hasText: email })).toContainText(role)

  const mail = await waitForMail(email)
  expect(mail.text).toContain('Welcome to the e2e run.')
  const link = /https?:\/\/\S+\/invitations\/accept[?#]token=\S+/.exec(mail.text)?.[0]
  expect(link, mail.text).toBeTruthy()
  return link!
}

/**
 * Accepts an invitation as the signed-in user of `page`: the link of the mail opens the accept page
 * of the web UI, and "Join the space" joins. Ends on the files of the space.
 */
export async function acceptInvitation(page: Page, link: string): Promise<void> {
  const url = new URL(link)
  expect(url.origin).toBe(new URL(env.webUrl).origin)
  await page.goto(url.pathname + url.search)
  await expect(page.getByRole('heading', { name: "You're invited to a space in Ruum" })).toBeVisible()
  await page.getByRole('button', { name: 'Join the space' }).click()
  await expect(toast(page, /^You've joined /)).toBeVisible()
  await page.waitForURL(/\/t\/[0-9a-f-]{36}\/files$/)
}

/** Opens the menu button `button` and chooses `item` (design-system 4.2 menus: row and "More" menus). */
export async function menuAction(page: Page, button: string | RegExp, item: string | RegExp): Promise<void> {
  await page.getByRole('button', { name: button }).click()
  const menu = page.getByRole('menu')
  await menu.getByRole('menuitem', { name: item }).or(menu.getByRole('menuitemradio', { name: item })).click()
}
