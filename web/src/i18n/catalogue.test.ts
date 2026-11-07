// The message catalogues (design 12.4): every translation has every key of the source, uses the
// same arguments, has the plural forms its language needs, and formats; the pseudo-locale changes
// every letter and makes text longer; the metadata says how each catalogue was made.
import {
  isArgumentElement,
  isDateElement,
  isNumberElement,
  isPluralElement,
  isSelectElement,
  isTagElement,
  isTimeElement,
  parse,
  type MessageFormatElement,
} from '@formatjs/icu-messageformat-parser'
import { describe, expect, it } from 'vitest'
import { LANGUAGES, PSEUDO_LOCALE } from './config'
import { makeIntl } from './intl'
import { pseudoMessage, pseudoMessages, pseudoText } from './pseudo'
import type { Catalogue } from './catalogue'

const files = import.meta.glob<Catalogue>('../locales/*.json', { eager: true, import: 'default' })
const catalogues = Object.fromEntries(Object.entries(files).map(([f, c]) => [/([\w-]+)\.json$/.exec(f)![1], c]))
const en = catalogues.en
const translations = Object.entries(catalogues).filter(([tag]) => tag !== 'en')

type Args = Map<string, 'plain' | 'number' | 'date' | 'plural' | 'select' | 'tag'>

/** The arguments of a message, by name and kind, and the option keys of each select. */
function argsOf(elements: MessageFormatElement[], args: Args = new Map(), selects = new Map<string, Set<string>>()) {
  for (const el of elements) {
    if (isArgumentElement(el)) args.set(el.value, args.get(el.value) ?? 'plain')
    else if (isNumberElement(el)) args.set(el.value, 'number')
    else if (isDateElement(el) || isTimeElement(el)) args.set(el.value, 'date')
    else if (isTagElement(el)) {
      args.set(el.value, 'tag')
      argsOf(el.children, args, selects)
    } else if (isPluralElement(el) || isSelectElement(el)) {
      args.set(el.value, isPluralElement(el) ? 'plural' : 'select')
      if (isSelectElement(el)) selects.set(el.value, new Set(Object.keys(el.options)))
      for (const o of Object.values(el.options)) argsOf(o.value, args, selects)
    }
  }
  return { args, selects }
}

/** The plural elements of a message with their option keys. */
function plurals(elements: MessageFormatElement[], out: string[][] = []): string[][] {
  for (const el of elements) {
    if (isPluralElement(el)) out.push(Object.keys(el.options))
    if (isPluralElement(el) || isSelectElement(el)) for (const o of Object.values(el.options)) plurals(o.value, out)
    if (isTagElement(el)) plurals(el.children, out)
  }
  return out
}

/** Values that make every branch-free path of a message format: numbers, the first select key, tags. */
function sampleValues(message: string) {
  const { args, selects } = argsOf(parse(message))
  const values: Record<string, unknown> = {}
  for (const [name, kind] of args) {
    if (kind === 'plural' || kind === 'number') values[name] = 3
    else if (kind === 'date') values[name] = new Date('2026-11-06T10:00:00Z')
    else if (kind === 'select') values[name] = [...(selects.get(name) ?? ['other'])][0]
    else if (kind === 'tag') values[name] = (chunks: string[]) => chunks.join('')
    else values[name] = `«${name}»`
  }
  return values
}

describe('message catalogues', () => {
  it('has a catalogue for every language of the configuration', () => {
    for (const l of LANGUAGES) expect(catalogues[l.tag], l.tag).toBeDefined()
  })

  it('keeps the metadata: the source is English, a translation says how it was made', () => {
    expect(en['@metadata']).toMatchObject({ locale: 'en', translation: 'source' })
    for (const [tag, c] of translations) {
      expect(c['@metadata'].locale).toBe(tag)
      expect(c['@metadata'].source).toBe('en')
      // A machine translation stays marked until a person checks it.
      expect(['machine', 'human']).toContain(c['@metadata'].translation)
    }
  })

  it('names keys by screen and meaning', () => {
    for (const key of Object.keys(en.messages)) expect(key, key).toMatch(/^[a-z][a-zA-Z]*(\.[a-zA-Z0-9_]+)+$/)
  })

  it.each(Object.keys(catalogues))('%s: every message is valid ICU and formats', (tag) => {
    const intl = makeIntl(tag, catalogues[tag].messages)
    for (const [key, message] of Object.entries(catalogues[tag].messages)) {
      expect(() => parse(message), `${tag} ${key}`).not.toThrow()
      const out = intl.formatMessage({ id: key }, sampleValues(en.messages[key] ?? message) as never)
      expect(typeof out === 'string' ? out : String(out), `${tag} ${key}`).not.toBe('')
    }
  })

  it.each(translations.map(([tag]) => tag))('%s: has every key of the source and no others', (tag) => {
    expect(Object.keys(catalogues[tag].messages).sort()).toEqual(Object.keys(en.messages).sort())
  })

  it.each(translations.map(([tag]) => tag))('%s: uses the same arguments, select keys and tags as the source', (tag) => {
    for (const [key, source] of Object.entries(en.messages)) {
      const want = argsOf(parse(source))
      const have = argsOf(parse(catalogues[tag].messages[key]))
      // A plain number may become a plural in a translation ("{n} files" -> "{n, plural, ...}").
      const kinds = (a: Args) =>
        Object.fromEntries([...a].map(([name, kind]) => [name, kind === 'plural' || kind === 'number' ? 'plain' : kind]))
      expect(kinds(have.args), `${tag} ${key}`).toEqual(kinds(want.args))
      for (const [name, keys] of want.selects)
        expect([...(have.selects.get(name) ?? [])].sort(), `${tag} ${key} select ${name}`).toEqual([...keys].sort())
    }
  })

  it.each(translations.map(([tag]) => tag))('%s: every plural has the forms the language needs', (tag) => {
    const needed = new Intl.PluralRules(tag).resolvedOptions().pluralCategories
    for (const [key, message] of Object.entries(catalogues[tag].messages)) {
      for (const forms of plurals(parse(message))) {
        for (const form of needed) expect(forms, `${tag} ${key}: plural form "${form}"`).toContain(form)
      }
    }
  })

  it('Estonian counts take the partitive after a number ("3 faili", not "3 failid")', () => {
    const intl = makeIntl('et', catalogues.et.messages)
    expect(intl.formatMessage({ id: 'uploads.toast.many' }, { n: 1 })).toBe('1 fail laaditi üles.')
    expect(intl.formatMessage({ id: 'uploads.toast.many' }, { n: 3 })).toBe('3 faili laaditi üles.')
    expect(intl.formatMessage({ id: 'format.when.minutes' }, { n: 5 })).toBe('5 minutit tagasi')
    expect(intl.formatMessage({ id: 'trash.empty.title' }, { n: 2 })).toBe('Kas kustutada 2 üksust jäädavalt?')
  })
})

describe('pseudo-locale', () => {
  it('accents every letter, keeps arguments and makes text about 40% longer', () => {
    expect(pseudoText('Share')).toBe('Šĥååŕéé')
    const intl = makeIntl(PSEUDO_LOCALE, { a: pseudoMessage('Moved {name} to {n, plural, one {# folder} other {# folders}}.') })
    expect(intl.formatMessage({ id: 'a' }, { name: 'notes.md', n: 2 })).toBe('[Ṁööṽééð notes.md ţöö 2 ƒööļðééŕš.]')
    const long = Object.values(en.messages).filter((m) => m.length > 40 && !m.includes('{'))
    const grown = long.map((m) => pseudoText(m).length / m.length)
    const average = grown.reduce((a, b) => a + b, 0) / grown.length
    expect(average).toBeGreaterThan(1.3)
    expect(average).toBeLessThan(1.6)
  })

  it('formats every message of the source with no plain letter left outside the arguments', () => {
    const intl = makeIntl(PSEUDO_LOCALE, pseudoMessages(en.messages))
    for (const [key, message] of Object.entries(en.messages)) {
      const values = sampleValues(message)
      const out = intl.formatMessage({ id: key }, values as never)
      const text = typeof out === 'string' ? out : String(out)
      // Arguments are «name»; everything else must be accented or not a letter.
      expect(text.replace(/«\w+»/g, ''), key).not.toMatch(/[A-Za-z]/)
      expect(text.startsWith('['), key).toBe(true)
    }
  })
})
