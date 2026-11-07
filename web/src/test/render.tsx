import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router'
import { vi } from 'vitest'
import { browser } from '../app/browser'
import { routes } from '../app/routes'
import { I18nProvider } from '../i18n'
import { ids } from '../mocks/db'

export { ids }

/** Paths of the two seed tenants. */
export const personal = `/t/${ids.personal}`
export const team = `/t/${ids.team}`

/**
 * Renders the whole app at `path` against the mock API. The locale is the browser's (jsdom:
 * `en-US`) unless `locale` sets the choice kept in the browser; signed-in pages follow the
 * user's preference of the mock API (`db.preferences`).
 */
export function renderApp(path: string, { locale }: { locale?: string | null } = {}) {
  const router = createMemoryRouter(routes, { initialEntries: [path] })
  const user = userEvent.setup()
  const assign = vi.spyOn(browser, 'assign').mockImplementation(() => {})
  const download = vi.spyOn(browser, 'download').mockImplementation(() => {})
  const view = render(
    <I18nProvider preference={locale}>
      <RouterProvider router={router} />
    </I18nProvider>,
  )
  return { ...view, router, user, assign, download }
}

/** The open dialog with this name. */
export function dialog(name: string | RegExp) {
  return screen.findByRole('dialog', { name })
}
