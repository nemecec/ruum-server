// The current react-intl instance, also for code outside components (error copy, dates, sizes,
// labels). I18nProvider sets it before it renders the app and renders the app again (with a new
// key) when the locale changes, so every text follows.
import type { ReactNode } from 'react'
import { createIntl, createIntlCache, type IntlShape } from 'react-intl'
import { sourceMessages, type MessageKey, type Messages } from './catalogue'
import { SOURCE_LANGUAGE } from './config'

export type Values = Record<string, string | number | boolean | Date | null | undefined>

export type RichValues = Record<string, ReactNode | Date | ((chunks: ReactNode[]) => ReactNode)>

const cache = createIntlCache()

function onError(err: Error) {
  // A broken message or a missing value is a bug: the tests fail on it.
  if (import.meta.env.MODE === 'test') throw err
  console.error(err)
}

export function makeIntl(locale: string, messages: Messages): IntlShape {
  return createIntl({ locale, defaultLocale: SOURCE_LANGUAGE, messages, onError }, cache)
}

let current = makeIntl(SOURCE_LANGUAGE, sourceMessages)

/** The react-intl instance of the current locale. */
export function currentIntl(): IntlShape {
  return current
}

export function setCurrentIntl(intl: IntlShape) {
  current = intl
}

/** A message of the catalogue as text, in the current locale. */
export function t(key: MessageKey, values?: Values): string {
  return current.formatMessage({ id: key }, values) as string
}

/** A message with tags or elements in it: `tr('x', { b: (c) => <strong>{c}</strong> })`. */
export function tr(key: MessageKey, values: RichValues): ReactNode {
  return current.formatMessage({ id: key }, values as Record<string, never>) as ReactNode
}

/** Labels from catalogue keys, read when used (so they follow the locale): `labels({a: 'x.a'}).a`. */
export function labels<K extends string>(keys: Record<K, MessageKey>): Record<K, string> {
  const out = {} as Record<K, string>
  for (const k of Object.keys(keys) as K[]) Object.defineProperty(out, k, { enumerable: true, get: () => t(keys[k]) })
  return out
}
