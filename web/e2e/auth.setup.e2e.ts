// Sign-in through the real OIDC flow (Zitadel login UI with a password). Keeps each user's
// signed-in browser state for the other specs.
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { api, signIn } from './support/app.ts'

for (const key of ['ann', 'ben'] as const) {
  test(`${key} signs in through Zitadel and gets a personal space`, async ({ page }) => {
    const user = readRun().users[key]
    await signIn(page, user)
    await expect(page).toHaveURL(/\/t\/[0-9a-f-]{36}\/files$/)
    await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
    await expect(page.getByRole('combobox', { name: 'Space' })).toContainText(`${user.displayName}'s space (personal)`)

    // The session is a cookie with a CSRF partner; /v1/me answers for this user.
    const me = await api<{ user: { email: string; email_verified: boolean } }>(page, 'GET', '/v1/me')
    expect(me.status).toBe(200)
    expect(me.body.user.email).toBe(user.email)
    expect(me.body.user.email_verified).toBe(true)
    const cookies = await page.context().cookies()
    expect(cookies.find((c) => c.name === 'ruum_session')?.httpOnly).toBe(true)
    expect(cookies.find((c) => c.name === 'ruum_csrf')?.httpOnly).toBe(false)

    await page.context().storageState({ path: storageStatePath(key) })
  })
}

// On a cold load of a protected page, /v1/me and /v1/tenants both answer 401; the second answer
// must not drop return_to (web/src/app/session.tsx).
test('a protected page without a session keeps return_to on the way to sign-in', async ({ page }) => {
  await page.goto('/account')
  await expect(page).toHaveURL(/\/signin\?return_to=%2Faccount$/)
})

test('sign-in with return_to comes back to that page', async ({ page }) => {
  const ann = readRun().users.ann
  await signIn(page, ann, '/signin?return_to=%2Faccount')
  await expect(page).toHaveURL(/\/t\/[0-9a-f-]{36}\/account$/)
  await expect(page.getByRole('heading', { name: 'Account', level: 1 })).toBeVisible()
})

test('sign out ends the session', async ({ page, baseURL }) => {
  const ann = readRun().users.ann
  const web = new URL(baseURL ?? 'http://localhost:5182').origin
  await signIn(page, ann)
  await page.getByRole('button', { name: 'Sign out' }).click()
  // The server ends the session and sends the browser through Zitadel's end-session endpoint and
  // back to the web UI, which sends a visitor without a session to /signin. Wait for the end of
  // that chain (not only for the web origin: the client-side step to /signin can still be under
  // way), so the next navigation does not race it (net::ERR_ABORTED).
  await page.waitForURL((u) => u.origin === web && u.pathname.startsWith('/signin'), { timeout: 15_000 })
  await page.waitForLoadState('load')
  await page.goto('/')
  await expect(page).toHaveURL(/\/signin/)
  expect((await api(page, 'GET', '/v1/me')).status).toBe(401)
})
