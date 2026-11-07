import { describe, expect, it } from 'vitest'
import { catalogueFor, directionOf, resolveLocale } from './config'

describe('locale choice (design 12.4)', () => {
  it('takes the preference first, then the browser, then English', () => {
    expect(resolveLocale('et', ['en-GB'])).toEqual({ locale: 'et', language: 'et' })
    expect(resolveLocale(null, ['et-EE', 'en'])).toEqual({ locale: 'et-EE', language: 'et' })
    expect(resolveLocale(undefined, ['fr-FR', 'en-GB'])).toEqual({ locale: 'en-GB', language: 'en' })
    expect(resolveLocale(null, ['fr', 'de'])).toEqual({ locale: 'en', language: 'en' })
    expect(resolveLocale(null, [])).toEqual({ locale: 'en', language: 'en' })
  })

  it('skips a preference without a catalogue', () => {
    expect(resolveLocale('fi', ['et'])).toEqual({ locale: 'et', language: 'et' })
  })

  it('knows the pseudo-locale', () => {
    expect(catalogueFor('en-XA')).toBe('en-XA')
    expect(resolveLocale(null, ['en-XA'])).toEqual({ locale: 'en-XA', language: 'en-XA' })
  })

  it('writes right-to-left languages right to left', () => {
    expect(directionOf('ar-EG')).toBe('rtl')
    expect(directionOf('et')).toBe('ltr')
  })
})
