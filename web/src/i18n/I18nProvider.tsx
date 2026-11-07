import { useCallback, useEffect, useMemo, useState, type ReactNode } from 'react'
import { RawIntlProvider, type IntlShape } from 'react-intl'
import { loadMessages, sourceMessages } from './catalogue'
import { browserLanguages, directionOf, resolveLocale, SOURCE_LANGUAGE } from './config'
import { makeIntl, setCurrentIntl } from './intl'
import { LocaleContext } from './useT'

/** The last language choice in this browser, so the sign-in page speaks it too. */
const STORAGE_KEY = 'ruum.locale'

function stored(): string | null {
  try {
    return localStorage.getItem(STORAGE_KEY)
  } catch {
    return null
  }
}

function store(tag: string | null) {
  try {
    if (tag) localStorage.setItem(STORAGE_KEY, tag)
    else localStorage.removeItem(STORAGE_KEY)
  } catch {
    // Storage may be off (private window); the choice still holds for this page.
  }
}

/**
 * The locale of the UI (design 12.4): the person's preference (from getMe, kept in this browser),
 * then the browser's languages, then English. Renders nothing until the catalogue is loaded,
 * and the whole tree again when the locale changes.
 */
export function I18nProvider({ children, preference: initial }: { children: ReactNode; preference?: string | null }) {
  const [preference, setPreferenceState] = useState<string | null>(() => (initial !== undefined ? initial : stored()))
  const { locale, language } = resolveLocale(preference, browserLanguages())
  const [intl, setIntl] = useState<IntlShape | null>(() =>
    language === SOURCE_LANGUAGE ? makeIntl(locale, sourceMessages) : null,
  )

  useEffect(() => {
    if (intl?.locale === locale) return
    let live = true
    void loadMessages(language).then((messages) => {
      if (live) setIntl(makeIntl(locale, messages))
    })
    return () => {
      live = false
    }
  }, [intl, locale, language])

  useEffect(() => {
    if (!intl) return
    document.documentElement.lang = intl.locale
    document.documentElement.dir = directionOf(intl.locale)
  }, [intl])

  const setPreference = useCallback((tag: string | null) => {
    store(tag)
    setPreferenceState(tag)
  }, [])

  const value = useMemo(
    () => ({ locale: intl?.locale ?? locale, language, preference, setPreference }),
    [intl, locale, language, preference, setPreference],
  )

  if (!intl) return null
  setCurrentIntl(intl)
  return (
    <LocaleContext.Provider value={value}>
      <RawIntlProvider value={intl} key={intl.locale}>
        {children}
      </RawIntlProvider>
    </LocaleContext.Provider>
  )
}
