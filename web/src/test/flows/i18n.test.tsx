// Localisation in the app (design 12.4): the language follows the user's preference from getMe,
// the Account page changes it on the server, pages outside a session follow the browser, and the
// pseudo-locale leaves no untranslated text on the main screens.
import { screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { db } from '../../mocks/db'
import { LANGUAGES } from '../../i18n'
import { personal, renderApp } from '../render'

/** The month and weekday names and day periods that Intl writes for a locale (not the catalogue's). */
function intlWords(locale: string): string[] {
  const out: string[] = []
  for (let m = 0; m < 12; m++) {
    const d = new Date(Date.UTC(2026, m, 1 + m, m * 2))
    for (const opts of [{ month: 'short' }, { month: 'long' }, { weekday: 'long' }, { hour: 'numeric', hour12: true }] as const)
      out.push(...new Intl.DateTimeFormat(locale, opts).formatToParts(d).map((p) => p.value))
  }
  return out
}

afterEach(() => {
  vi.restoreAllMocks()
  document.documentElement.lang = ''
})

/** Visible text and accessible names in `root` with plain ASCII words that are not test data. */
function untranslated(root: HTMLElement, data: string[]): string[] {
  const allowed = new Set(data.flatMap((d) => String(d ?? '').toLowerCase().match(/[a-z]{2,}/g) ?? []))
  const texts: string[] = []
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT)
  for (let n = walker.nextNode(); n; n = walker.nextNode()) texts.push(n.textContent ?? '')
  for (const el of Array.from(root.querySelectorAll('[aria-label],[placeholder],[title]')))
    for (const a of ['aria-label', 'placeholder', 'title']) texts.push(el.getAttribute(a) ?? '')
  return texts.filter((t) => (t.toLowerCase().match(/[a-z]{2,}/g) ?? []).some((w) => !allowed.has(w)))
}

describe('localisation', () => {
  it('shows the app in the language of the user preference', async () => {
    db.preferences = { locale: 'et' }
    renderApp(`${personal}/files`)
    expect(await screen.findByRole('link', { name: 'Minuga jagatud' })).toBeInTheDocument()
    expect(await screen.findByRole('heading', { name: 'Failid', level: 1 })).toBeInTheDocument()
    expect(document.documentElement.lang).toBe('et')
  })

  it('changes the language on the Account page and stores it with the account', async () => {
    const { user } = renderApp(`${personal}/account`)
    const select = await screen.findByRole('combobox', { name: 'Language of Ruum' })
    // The choice applies with Save, not on input (WCAG 3.2.2).
    await user.selectOptions(select, 'et')
    expect(db.preferences.locale).toBeNull()
    await user.click(within(select.closest('form')!).getByRole('button', { name: 'Save' }))
    expect(await screen.findByRole('heading', { name: 'Konto', level: 1 })).toBeInTheDocument()
    expect(db.preferences.locale).toBe('et')
    // The focus comes back to the picker of the new page, which says what changed (A1).
    const picker = await screen.findByRole('combobox', { name: 'Kasutajaliidese keel' })
    await waitFor(() => expect(picker).toHaveFocus())
    expect(screen.getByText('Tehtud. Ruumi keel on nüüd: Eesti.')).toBeInTheDocument()
    // "Same as the browser" clears the preference; jsdom's browser speaks English.
    await user.selectOptions(picker, '')
    await user.click(within(picker.closest('form')!).getByRole('button', { name: 'Salvesta' }))
    expect(await screen.findByRole('heading', { name: 'Account', level: 1 })).toBeInTheDocument()
    expect(db.preferences.locale).toBeNull()
  })

  it('follows the browser before sign-in', async () => {
    vi.spyOn(navigator, 'languages', 'get').mockReturnValue(['et-EE', 'en'])
    renderApp('/signin')
    expect(await screen.findByRole('heading', { name: 'Logi Ruumi sisse' })).toBeInTheDocument()
  })

  it('writes dates and sizes the way the language does', async () => {
    db.preferences = { locale: 'et' }
    renderApp(`${personal}/files`)
    const table = await screen.findByRole('table', { name: 'Selle kausta sisu' })
    // Estonian decimals use a comma ("1,5 KB").
    await waitFor(() => expect(within(table).getAllByText(/^\d+(,\d)? (B|KB|MB)$/).length).toBeGreaterThan(0))
    expect(within(table).queryAllByText(/^\d+\.\d (KB|MB)$/)).toHaveLength(0)
  })

  it.each([`${personal}/files`, `${personal}/agents`, `${personal}/account`, `${personal}/trash`, '/signin'])(
    'pseudo-locale: %s has no untranslated text',
    async (path) => {
      db.preferences = { locale: 'en-XA' }
      vi.spyOn(navigator, 'languages', 'get').mockReturnValue(['en-XA'])
      const { container } = renderApp(path)
      await waitFor(() => expect(container.querySelector('h1')?.textContent).toMatch(/^\[/))
      await waitFor(() => expect(screen.queryAllByRole('status').filter((s) => /\.\.\.\]?$/.test(s.textContent ?? ''))).toHaveLength(0))
      const data = [
        db.user.display_name,
        db.user.email,
        ...db.nodes.map((n) => n.name),
        ...db.agents.map((a) => a.name),
        ...db.sessions.map((s) => s.user_agent ?? ''),
        ...db.appPasswords.map((p) => `${p.name} ${p.prefix}`),
        ...db.tenants.map((t) => JSON.stringify(t)),
        ...db.members.map((m) => `${m.display_name} ${m.email ?? ''}`),
        ...db.identities.map((i) => `${i.provider ?? ''} ${i.email ?? ''}`),
        // Brand and product names and the names of the languages stay as they are; dates come from Intl.
        'Ruum Google Microsoft Passkey KB MB GB',
        ...LANGUAGES.map((l) => l.name),
        ...intlWords('en-XA'),
      ]
      expect(untranslated(container, data)).toEqual([])
    },
  )
})
