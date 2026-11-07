import { createContext, useContext, useMemo, type ReactNode } from 'react'
import { useIntl } from 'react-intl'
import type { MessageKey } from './catalogue'
import type { RichValues, Values } from './intl'

export type Locale = {
  /** The `Intl` locale in use (`en-GB`, `et`). */
  locale: string
  /** The catalogue in use (`en`, `et`, `en-XA`). */
  language: string
  /** The person's choice: a tag, or null to follow the browser. */
  preference: string | null
  /** Changes the choice (after the server stored it). */
  setPreference: (tag: string | null) => void
}

export const LocaleContext = createContext<Locale | null>(null)

/** The locale and the person's language choice. Only inside I18nProvider. */
export function useLocale(): Locale {
  const l = useContext(LocaleContext)
  if (!l) throw new Error('useLocale outside I18nProvider')
  return l
}

export type T = ((key: MessageKey, values?: Values) => string) & {
  /** A message with tags or elements in it: `t.rich('x', { b: (c) => <strong>{c}</strong> })`. */
  rich: (key: MessageKey, values: RichValues) => ReactNode
}

/** The messages of the catalogue in the current locale. */
export function useT(): T {
  const intl = useIntl()
  return useMemo(() => {
    const text = (key: MessageKey, values?: Values) => intl.formatMessage({ id: key }, values) as string
    const rich = (key: MessageKey, values: RichValues) => intl.formatMessage({ id: key }, values as Record<string, never>) as ReactNode
    return Object.assign(text, { rich }) as T
  }, [intl])
}
