// Sign-in through an external identity provider: the mock OIDC provider of the compose profile
// `sso-mock` (scripts/dev.sh up --sso-mock), which scripts/zitadel_idps.sh adds to Zitadel as
// "Mock SSO". It stands in for Google and Microsoft, which a person checks by hand
// (docs/DEVSTACK.md "Sign-in providers"). The flow is the real one: the sign-in page of the UI,
// `loginStart`, the Zitadel login UI, the provider's sign-in page, Zitadel's callback (the account
// is made on the first sign-in), `loginCallback` and the session cookie.
//
// Without the mock provider the tests are skipped, unless RUUM_E2E_SSO_MOCK=required (CI).
import crypto from 'node:crypto'
import { expect, test, type Page } from '@playwright/test'
import { env } from './support/env.ts'
import { api } from './support/app.ts'
import { loginPolicyIdp, userWithLinks } from './support/zitadel.ts'

const PROVIDER = 'Mock SSO'

interface Person {
  subject: string
  email: string
  givenName: string
  familyName: string
}

/** Signs in with the button of the mock provider on the Zitadel login page, as `person`. */
async function signInWithMock(page: Page, person: Person) {
  const home = new URL(env.webUrl).origin
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Sign in to Ruum' })).toBeVisible()
  // The UI's buttons all end on the Zitadel login UI (the `idp` hint is not sent yet).
  await page.getByRole('link', { name: 'Sign in with email' }).click()
  await page.waitForURL(/\/ui\/login\//)
  await page.getByRole('link', { name: PROVIDER }).click()
  // The provider's own sign-in page (mock-oauth2-server): any subject, and the claims to send.
  await page.waitForURL((u) => u.hostname === 'oidc-mock.localhost')
  await page.locator('input[name=username]').fill(person.subject)
  await page.locator('textarea[name=claims]').fill(
    JSON.stringify({
      email: person.email,
      email_verified: true,
      given_name: person.givenName,
      family_name: person.familyName,
      name: `${person.givenName} ${person.familyName}`,
      preferred_username: person.email,
    }),
  )
  await page.getByRole('button', { name: 'Sign-in' }).click()
  // A new account is asked once to set up a second factor; the login policy allows skipping it.
  const skip = page.getByRole('button', { name: 'Skip' })
  await Promise.race([
    skip
      .waitFor({ state: 'visible', timeout: 20_000 })
      .then(() => skip.click())
      .catch(() => {}),
    page.waitForURL((u) => u.origin === home, { timeout: 20_000 }).catch(() => {}),
  ])
  await page.waitForURL((u) => u.origin === home && !u.pathname.startsWith('/signin'), { timeout: 20_000 })
}

test.describe('sign-in through an external identity provider', () => {
  test.beforeAll(async () => {
    const id = await loginPolicyIdp(PROVIDER)
    if (!id && process.env.RUUM_E2E_SSO_MOCK === 'required') {
      throw new Error(`"${PROVIDER}" is not on the Zitadel login page; run scripts/dev.sh up --sso-mock`)
    }
    test.skip(!id, `"${PROVIDER}" is not on the Zitadel login page (scripts/dev.sh up --sso-mock)`)
  })

  test('a new person signs in with the provider, gets an account, and comes back to the same account', async ({ page, browser, baseURL }) => {
    const run = crypto.randomBytes(3).toString('hex')
    const person: Person = { subject: `sso-${run}`, email: `sso-${run}@example.test`, givenName: 'Sam', familyName: run }
    const displayName = `${person.givenName} ${person.familyName}`

    await signInWithMock(page, person)
    await expect(page).toHaveURL(/\/t\/[0-9a-f-]{36}\/files$/)
    await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
    await expect(page.getByRole('button', { name: `${displayName}'s space (change space)` })).toBeVisible()

    // The server knows the person by the provider's verified email and name.
    const me = await api<{ user: { id: string; email: string; email_verified: boolean; display_name: string } }>(page, 'GET', '/v1/me')
    expect(me.status).toBe(200)
    expect(me.body.user.email).toBe(person.email)
    expect(me.body.user.email_verified).toBe(true)
    expect(me.body.user.display_name).toBe(displayName)

    // Zitadel made one user, linked to the provider's subject (no password of its own).
    const providerId = await loginPolicyIdp(PROVIDER)
    const zuser = await userWithLinks(person.email)
    expect(zuser?.emailVerified).toBe(true)
    expect(zuser?.links).toEqual([expect.objectContaining({ idpId: providerId, userId: person.subject })])

    // Sign out, and sign in again with the same provider account: the same Ruum user.
    const web = new URL(baseURL ?? env.webUrl).origin
    await page.getByRole('button', { name: 'Sign out' }).click()
    await page.waitForURL((u) => u.origin === web && u.pathname.startsWith('/signin'), { timeout: 15_000 })
    await page.waitForLoadState('load')
    expect((await api(page, 'GET', '/v1/me')).status).toBe(401)

    // In a new browser (no Zitadel cookies left, so the login page shows the providers again).
    const context = await browser.newContext({ baseURL })
    try {
      const fresh = await context.newPage()
      await signInWithMock(fresh, person)
      await expect(fresh).toHaveURL(/\/t\/[0-9a-f-]{36}\/files$/)
      const again = await api<{ user: { id: string } }>(fresh, 'GET', '/v1/me')
      expect(again.status).toBe(200)
      expect(again.body.user.id).toBe(me.body.user.id)
      expect((await userWithLinks(person.email))?.userId).toBe(zuser?.userId)
    } finally {
      await context.close()
    }
  })
})
