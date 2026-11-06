// Axe on the main screens with the real server and Ann's data (card UX-15): no serious or
// critical violations of the WCAG 2.2 A and AA rules, at desktop and phone width, light and dark.
// e2e/visual.e2e.ts runs the same check on the mock data, with snapshots.
import AxeBuilder from '@axe-core/playwright'
import { expect, test, type Page } from '@playwright/test'
import { storageStatePath } from './support/env.ts'
import { tenantOf } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

async function axe(page: Page, screen: string) {
  await expect(page.getByRole('status').filter({ hasText: /\.\.\.$/ })).toHaveCount(0)
  const result = await new AxeBuilder({ page }).withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa']).analyze()
  const bad = result.violations
    .filter((v) => v.impact === 'serious' || v.impact === 'critical')
    .map((v) => `${v.id} (${v.impact}): ${v.help} - ${v.nodes.map((n) => n.target.join(' ')).slice(0, 3).join(', ')}`)
  expect(bad, `axe on ${screen}`).toEqual([])
}

const variants = [
  { name: 'desktop light', viewport: { width: 1280, height: 800 }, colorScheme: 'light' as const },
  { name: 'desktop dark', viewport: { width: 1280, height: 800 }, colorScheme: 'dark' as const },
  { name: 'phone light', viewport: { width: 390, height: 844 }, colorScheme: 'light' as const },
  { name: 'phone dark', viewport: { width: 390, height: 844 }, colorScheme: 'dark' as const },
]

for (const v of variants) {
  test(`main screens have no serious or critical axe violations (${v.name})`, async ({ page }) => {
    await page.setViewportSize(v.viewport)
    await page.emulateMedia({ colorScheme: v.colorScheme })
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
    const t = `/t/${tenantOf(page)}`
    await axe(page, 'files')
    for (const [path, heading] of [
      ['shared', 'Shared with me'],
      ['trash', 'Trash'],
      ['search?q=e2e', 'Search'],
      ['agents', 'Agents'],
      ['agents/connect', 'Connect an agent'],
      ['account', 'Account'],
      ['admin/members', /^Space settings/],
      ['admin/invitations', /^Space settings/],
      ['admin/policy', /^Space settings/],
      ['admin/limits', /^Space settings/],
      ['admin/audit', /^Space settings/],
    ] as const) {
      await page.goto(`${t}/${path}`)
      await expect(page.getByRole('heading', { name: heading, level: 1 })).toBeVisible()
      await axe(page, path)
    }
    // A dialog: New folder.
    await page.goto(`${t}/files`)
    await page.getByRole('button', { name: 'New folder' }).first().click()
    await expect(page.getByRole('dialog', { name: 'New folder' })).toBeVisible()
    await axe(page, 'new folder dialog')
  })
}

test('sign-in and a share link without an account have no serious or critical axe violations', async ({ browser }) => {
  const context = await browser.newContext()
  const page = await context.newPage()
  await page.goto('/signin')
  await expect(page.getByRole('heading', { name: 'Sign in to Ruum' })).toBeVisible()
  await axe(page, 'sign-in')
  await page.goto('/s#ruum_link_this-link-does-not-exist')
  await expect(page.getByRole('heading', { name: "This link doesn't work any more" })).toBeVisible()
  await axe(page, 'ended share link')
  await context.close()
})
