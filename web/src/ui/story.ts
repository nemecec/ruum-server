// The sample text of the Ladle stories (design 12.5, UX panel M1). Stories show content (names,
// notes, labels) that the app takes from the catalogue or from data. `sample()` puts that sample text
// through the pseudo-locale when the catalogue runs with `&locale=en-XA`, so the catalogue shows
// every component with long, accented text and cut-off text shows up at component level. In the
// other locales the sample text stays as written. Only for stories.
import { pseudo } from '../i18n/pseudo'
import { PSEUDO_LOCALE } from '../i18n/config'

function pseudoOn(): boolean {
  return typeof window !== 'undefined' && new URLSearchParams(window.location.search).get('locale') === PSEUDO_LOCALE
}

/** A sample text of a story, in the pseudo-locale when the catalogue shows it. */
export function sample(text: string): string {
  return pseudoOn() ? pseudo(text) : text
}
