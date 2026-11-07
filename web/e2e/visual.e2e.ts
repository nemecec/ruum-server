// Visual snapshots and axe checks of the main screens (card UX-15, design 18.4.1), at desktop and
// phone width, in light and dark. The UI runs with the mock API (src/mocks) and a fixed clock, so
// the screens are the same on every run. An agent's change that moves a screen shows up as a
// snapshot diff; a change that breaks accessibility fails the axe check (no serious or critical
// violations).
//
// Update the snapshots after an intended change:
//   RUUM_E2E_MOCK_ONLY=1 npx playwright test --project visual --update-snapshots
//
// One more pass runs the main screens in the pseudo-locale (design 12.4: accented, about 40%
// longer text), at desktop width in light: it catches layouts that break with longer text, and
// text that does not come from the message catalogue.
import AxeBuilder from '@axe-core/playwright'
import { expect, test, type Page } from '@playwright/test'
import { english, pseudoWords, untranslated, type Words } from './support/pseudo.ts'

const NOW = new Date('2026-11-06T10:00:00Z')

const variants = [
  { name: 'desktop-light', viewport: { width: 1280, height: 800 }, colorScheme: 'light' as const },
  { name: 'desktop-dark', viewport: { width: 1280, height: 800 }, colorScheme: 'dark' as const },
  { name: 'phone-light', viewport: { width: 390, height: 844 }, colorScheme: 'light' as const },
  { name: 'phone-dark', viewport: { width: 390, height: 844 }, colorScheme: 'dark' as const },
]

// The seed of the mock API (src/mocks/db.ts).
const personal = '/t/0190a000-0000-7000-8000-000000000001'
const team = '/t/0190a000-0000-7000-8000-000000000002'
const documents = '0190a000-0000-7000-8000-000000000201'
const photos = '0190a000-0000-7000-8000-000000000202'
const claude = '0190a000-0000-7000-8000-000000000301'

/** Waits until no region says "Loading..." and the mock API's latency has passed. */
async function settle(page: Page) {
  await expect(page.getByRole('status').filter({ hasText: /\.\.\.\]?$/ })).toHaveCount(0)
  await page.waitForTimeout(400)
  await expect(page.getByRole('status').filter({ hasText: /\.\.\.\]?$/ })).toHaveCount(0)
}

/** No serious or critical axe violations (WCAG 2.2 A and AA rules). */
async function axe(page: Page, screen: string) {
  const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']).analyze()
  const bad = result.violations
    .filter((v) => v.impact === 'serious' || v.impact === 'critical')
    .map((v) => `${v.id} (${v.impact}): ${v.help} - ${v.nodes.map((n) => n.target.join(' ')).slice(0, 3).join(', ')}`)
  expect(bad, `axe on ${screen}`).toEqual([])
}

type Screen = { name: string; open: (page: Page, words: Words) => Promise<void> }

/** A consent request in the shape the server seals (ruum-core identity::tokens::Sealed): the claims are inside `v`. */
const b64 = (o: object) => Buffer.from(JSON.stringify(o)).toString('base64url')
const consentRequest = `${b64({ alg: 'ES256', typ: 'JWT' })}.${b64({
  iss: 'http://localhost',
  aud: 'ruum:oauth_request',
  iat: 1,
  exp: 2,
  v: { client_id: 'claude-desktop', redirect_uri: 'https://claude.example/callback' },
})}.sig`

const go = (path: string) => async (page: Page) => {
  await page.goto(path)
}

const screens: Screen[] = [
  { name: 'files', open: go(`${personal}/files`) },
  {
    name: 'details',
    open: async (page) => {
      await page.goto(`${personal}/files?node=0190a000-0000-7000-8000-000000000203`)
    },
  },
  {
    name: 'versions',
    open: async (page) => {
      await page.goto(`${personal}/files?node=0190a000-0000-7000-8000-000000000203&panel=versions`)
    },
  },
  {
    name: 'compare',
    open: async (page, w) => {
      await page.goto(`${personal}/files?node=0190a000-0000-7000-8000-000000000203&panel=versions`)
      await settle(page)
      await page.getByRole('button', { name: w.prefix('Compare version of') }).last().click()
      await expect(
        page.getByRole('dialog', { name: w.exact('Compare versions') }).getByRole('group', { name: w.exact('Changes') }),
      ).toBeVisible()
    },
  },
  {
    name: 'share',
    open: async (page, w) => {
      await page.goto(`${personal}/files?node=0190a000-0000-7000-8000-000000000203`)
      await settle(page)
      await page.getByRole('button', { name: w.exact('Share'), exact: true }).click()
      await expect(page.getByRole('dialog', { name: w.prefix('Share ') })).toBeVisible()
    },
  },
  { name: 'empty-folder', open: go(`${personal}/files/${photos}`) },
  { name: 'folder', open: go(`${personal}/files/${documents}`) },
  { name: 'shared', open: go(`${personal}/shared`) },
  { name: 'trash', open: go(`${personal}/trash`) },
  { name: 'search', open: go(`${personal}/search?q=plan`) },
  { name: 'agents', open: go(`${personal}/agents`) },
  { name: 'connect', open: go(`${personal}/agents/connect`) },
  { name: 'agent', open: go(`${personal}/agents/${claude}`) },
  { name: 'account', open: go(`${personal}/account`) },
  { name: 'members', open: go(`${team}/admin/members`) },
  { name: 'invitations', open: go(`${personal}/admin/invitations`) },
  { name: 'activity-log', open: go(`${team}/admin/audit`) },
  { name: 'consent', open: go(`/oauth/consent?request=${consentRequest}`) },
  { name: 'accept-invitation', open: go('/invitations/accept?token=demo-invite') },
  { name: 'share-link', open: go('/s#demo-folder') },
  { name: 'share-link-password', open: go('/s#demo-locked') },
  { name: 'not-found', open: go('/nowhere') },
]

for (const v of variants) {
  test.describe(v.name, () => {
    test.use({ viewport: v.viewport, colorScheme: v.colorScheme })

    test.beforeEach(async ({ page }) => {
      await page.clock.setFixedTime(NOW)
    })

    test('sign-in', async ({ page }) => {
      await page.goto('/signin')
      await expect(page.getByRole('heading', { name: 'Sign in to Ruum' })).toBeVisible()
      await axe(page, 'sign-in')
      await expect(page).toHaveScreenshot(`sign-in-${v.name}.png`)
    })

    for (const s of screens) {
      test(s.name, async ({ page, context }) => {
        // The mock sign-in (mock-auth.ts) sets the session cookie of the mock API.
        await context.addCookies([
          { name: 'ruum_mock_session', value: '1', domain: 'localhost', path: '/' },
          { name: 'ruum_csrf', value: 'visual-test', domain: 'localhost', path: '/' },
        ])
        await s.open(page, english)
        await settle(page)
        await axe(page, s.name)
        await expect(page).toHaveScreenshot(`${s.name}-${v.name}.png`)
      })
    }
  })
}

test.describe('pseudo-locale', () => {
  test.use({ viewport: { width: 1280, height: 800 }, colorScheme: 'light', locale: 'en-XA' })

  test.beforeEach(async ({ page }) => {
    await page.clock.setFixedTime(NOW)
  })

  test('sign-in', async ({ page }) => {
    await page.goto('/signin')
    await expect(page.getByRole('heading', { name: pseudoWords.exact('Sign in to Ruum') })).toBeVisible()
    expect(await untranslated(page)).toEqual([])
    await expect(page).toHaveScreenshot('sign-in-pseudo.png')
  })

  for (const s of screens) {
    test(s.name, async ({ page, context }) => {
      await context.addCookies([
        { name: 'ruum_mock_session', value: '1', domain: 'localhost', path: '/' },
        { name: 'ruum_csrf', value: 'visual-test', domain: 'localhost', path: '/' },
      ])
      await s.open(page, pseudoWords)
      await settle(page)
      await expect(page.locator('html')).toHaveAttribute('lang', 'en-XA')
      expect(await untranslated(page), `untranslated text on ${s.name}`).toEqual([])
      await axe(page, `${s.name} (pseudo-locale)`)
      await expect(page).toHaveScreenshot(`${s.name}-pseudo.png`)
    })
  }
})
