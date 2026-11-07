import type { GlobalProvider } from '@ladle/react'
import { MemoryRouter } from 'react-router'
import { I18nProvider } from '../src/i18n'
import '../src/ui/global.css'

/**
 * The locale of the catalogue: `&locale=et` or `&locale=en-XA` (the pseudo-locale, design 12.4)
 * in the address of a story; the browser's language otherwise.
 */
function locale(): string | undefined {
  return new URLSearchParams(window.location.search).get('locale') ?? undefined
}

// Every story runs inside a router (links and nav items need one) and the message catalogue, on
// the page background.
export const Provider: GlobalProvider = ({ children }) => (
  <I18nProvider preference={locale()}>
    <MemoryRouter>{children}</MemoryRouter>
  </I18nProvider>
)
