// The languages of the UI (design 12.4). English is the source; every other language is a
// catalogue in src/locales/<tag>.json. Adding a language: add its catalogue and one entry here.

export type Language = {
  /** The BCP 47 tag of the catalogue (`et`). */
  tag: string
  /** The name of the language in that language, for the language picker. Not translated. */
  name: string
}

/** The language of the source catalogue and the last fallback. */
export const SOURCE_LANGUAGE = 'en'

/**
 * The pseudo-locale of the tests: every message of the source catalogue with accented letters,
 * about 40% longer and in brackets, so untranslated text and layouts that break with longer text
 * show up. Its catalogue is made from the English one at run time.
 */
export const PSEUDO_LOCALE = 'en-XA'

/** The languages a person can pick. */
export const LANGUAGES: Language[] = [
  { tag: 'en', name: 'English' },
  { tag: 'et', name: 'Eesti' },
]

/** Languages written right to left, for `<html dir>`. None of ours yet; CSS uses logical properties. */
const RTL = new Set(['ar', 'fa', 'he', 'ur'])

/** The catalogue for a language tag, or null when there is none (`en-GB` has `en`, `fr` has none). */
export function catalogueFor(tag: string): string | null {
  if (tag === PSEUDO_LOCALE) return PSEUDO_LOCALE
  const primary = tag.toLowerCase().split('-')[0]
  return LANGUAGES.some((l) => l.tag === primary) ? primary : null
}

export type Resolved = {
  /** The locale for `Intl` (dates, numbers): the full tag that was picked, such as `en-GB`. */
  locale: string
  /** The catalogue: `en`, `et` or the pseudo-locale. */
  language: string
}

/**
 * The locale of the UI: the person's preference, then the browser's languages in order, then
 * English. The first one with a catalogue wins. Its full tag stays the `Intl` locale, so English
 * in a British browser writes dates the British way.
 */
export function resolveLocale(preference: string | null | undefined, browser: readonly string[]): Resolved {
  for (const tag of [preference, ...browser]) {
    if (!tag) continue
    const language = catalogueFor(tag)
    if (language) return { locale: validTag(tag) ?? language, language }
  }
  return { locale: SOURCE_LANGUAGE, language: SOURCE_LANGUAGE }
}

/** The tag in canonical form, or null when `Intl` does not take it. */
function validTag(tag: string): string | null {
  try {
    return Intl.getCanonicalLocales(tag)[0] ?? null
  } catch {
    return null
  }
}

/** The text direction of a locale. */
export function directionOf(locale: string): 'ltr' | 'rtl' {
  return RTL.has(locale.toLowerCase().split('-')[0]) ? 'rtl' : 'ltr'
}

/** The browser's languages, most wanted first. */
export function browserLanguages(): readonly string[] {
  if (typeof navigator === 'undefined') return []
  return navigator.languages?.length ? navigator.languages : navigator.language ? [navigator.language] : []
}
