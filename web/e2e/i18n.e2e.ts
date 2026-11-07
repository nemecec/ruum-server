// Localisation against the real server (design 12.4): one pass of the main screens in the
// pseudo-locale, and the language preference stored with the account (updateMyPreferences).
import { expect, test, type Page } from '@playwright/test'
import { storageStatePath } from './support/env.ts'
import { api } from './support/app.ts'
import { pseudo, untranslated } from './support/pseudo.ts'

type Me = { preferences?: { locale: string | null } }

test.describe('pseudo-locale', () => {
  test.use({ storageState: storageStatePath('ann'), locale: 'en-XA' })

  test('the main screens have no untranslated text', async ({ page }) => {
    await page.goto('/')
    await expect(page.getByRole('heading', { name: pseudo('Files'), level: 1 })).toBeVisible()
    await expect(page.locator('html')).toHaveAttribute('lang', 'en-XA')
    const nav = page.getByRole('navigation', { name: pseudo('Main') })
    const screens: [string, string][] = [
      ['Files', 'Files'],
      ['Shared with me', 'Shared with me'],
      ['Trash', 'Trash'],
      ['Agents', 'Agents'],
    ]
    for (const [link, heading] of screens) {
      await nav.getByRole('link', { name: pseudo(link) }).click()
      await expect(page.getByRole('heading', { name: pseudo(heading), level: 1 })).toBeVisible()
      await settle(page)
      expect(await untranslated(page), `untranslated text on ${link}`).toEqual([])
    }
    await page.goto('/account')
    await expect(page.getByRole('heading', { name: pseudo('Account'), level: 1 })).toBeVisible()
    await settle(page)
    expect(await untranslated(page), 'untranslated text on Account').toEqual([])
  })
})

test.describe('language preference', () => {
  test.use({ storageState: storageStatePath('ann') })

  test.afterEach(async ({ page }) => {
    // The other specs read the UI in English.
    await api(page, 'PATCH', '/v1/me/preferences', { body: { locale: null } })
  })

  test('the language chosen on the Account page is stored with the account', async ({ page }) => {
    await page.goto('/account')
    await expect(page.getByRole('heading', { name: 'Account', level: 1 })).toBeVisible()
    await page.getByRole('combobox', { name: 'Language of Ruum' }).selectOption('et')
    await expect(page.getByRole('heading', { name: 'Konto', level: 1 })).toBeVisible()
    expect((await api<Me>(page, 'GET', '/v1/me')).body.preferences?.locale).toBe('et')

    // A new browser (no local choice) gets the language from the server.
    await page.evaluate(() => localStorage.clear())
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Failid', level: 1 })).toBeVisible()

    // "Same as the browser" clears it.
    await page.goto('/account')
    await page.getByRole('combobox', { name: 'Ruumi keel' }).selectOption('')
    await expect(page.getByRole('heading', { name: 'Account', level: 1 })).toBeVisible()
    expect((await api<Me>(page, 'GET', '/v1/me')).body.preferences?.locale).toBeNull()
  })

  test('a tag that is not BCP 47 is refused', async ({ page }) => {
    await page.goto('/account')
    const res = await api<{ error: { code: string } }>(page, 'PATCH', '/v1/me/preferences', { body: { locale: 'Estonian' } })
    expect(res.status).toBe(422)
    expect(res.body.error.code).toBe('validation_failed')
  })
})

/** Waits until no region says "Loading..." (in any locale). */
async function settle(page: Page) {
  await expect(page.getByRole('status').filter({ hasText: /\.\.\.\]?$/ })).toHaveCount(0)
}
