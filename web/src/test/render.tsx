import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { createMemoryRouter, RouterProvider } from 'react-router'
import { vi } from 'vitest'
import { browser } from '../app/browser'
import { routes } from '../app/routes'
import { ids } from '../mocks/db'

export { ids }

/** Paths of the two seed tenants. */
export const personal = `/t/${ids.personal}`
export const team = `/t/${ids.team}`

/** Renders the whole app at `path` against the mock API. */
export function renderApp(path: string) {
  const router = createMemoryRouter(routes, { initialEntries: [path] })
  const user = userEvent.setup()
  const assign = vi.spyOn(browser, 'assign').mockImplementation(() => {})
  const download = vi.spyOn(browser, 'download').mockImplementation(() => {})
  const view = render(<RouterProvider router={router} />)
  return { ...view, router, user, assign, download }
}

/** The open dialog with this name. */
export function dialog(name: string | RegExp) {
  return screen.findByRole('dialog', { name })
}
