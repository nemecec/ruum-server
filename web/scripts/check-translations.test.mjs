// The checks of the translation pipeline (scripts/check-translations.mjs, design 12.4.1 step 3):
// the real catalogues pass, and each rule catches what it is for.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { changedKeys, checkCatalogue, parseGlossary, sourceHash } from './check-translations.mjs'

const web = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
const read = (p) => JSON.parse(fs.readFileSync(path.join(web, p), 'utf8'))
const markdown = fs.readFileSync(path.join(web, '..', 'docs', 'ux', 'glossary.md'), 'utf8')
const glossary = parseGlossary(markdown, 'Estonian')

/** A one-message Estonian catalogue with a valid status for the English text. */
function check(english, estonian, meta = {}) {
  const source = { 'x.y': english }
  const catalogue = {
    '@metadata': { locale: 'et', source: 'en', translation: 'machine', status: { 'x.y': `machine:${sourceHash(english)}` }, ...meta },
    messages: { 'x.y': estonian },
  }
  return checkCatalogue({ tag: 'et', source, catalogue, glossary }).errors.map((e) => e.rule)
}

describe('check-translations', () => {
  it('passes the real catalogues', () => {
    const source = read('src/locales/en.json').messages
    const context = read('src/locales/context/en.json').messages
    for (const file of fs.readdirSync(path.join(web, 'src/locales')).filter((f) => f.endsWith('.json') && f !== 'en.json')) {
      const tag = file.slice(0, -5)
      const name = new Intl.DisplayNames(['en'], { type: 'language' }).of(tag)
      const { errors } = checkCatalogue({ tag, source, catalogue: read(`src/locales/${file}`), glossary: parseGlossary(markdown, name), context })
      expect(errors, tag).toEqual([])
    }
  })

  it('reads the stems of the Estonian column and the do-not-translate list', () => {
    expect(glossary.terms).toContainEqual({ term: 'folder', stems: ['kaust'] })
    expect(glossary.terms).toContainEqual({ term: 'agent', stems: ['agent', 'agend'] })
    expect(glossary.keep).toEqual(expect.arrayContaining(['WebDAV', 'Claude Code', 'WHERE', 'CREATE TABLE']))
    expect(glossary.keep).not.toContain('SQL keywords')
  })

  it('accepts a good translation', () => {
    expect(check('Moved {name} to {folder}.', '{name} teisaldati kausta {folder}.')).toEqual([])
  })

  it('catches invalid ICU', () => {
    expect(check('Moved {name}.', '{name teisaldati.')).toEqual(['icu'])
  })

  it('catches a missing or renamed placeholder, select key or tag', () => {
    expect(check('Moved {name} to {folder}.', '{name} teisaldati kausta {kaust}.')).toContain('arguments')
    expect(check('{x, select, yes {Yes} other {No}}', '{x, select, jah {Jah} other {Ei}}')).toContain('arguments')
    expect(check('As <b>{name}</b>.', 'Kasutajana {name}.')).toContain('arguments')
  })

  it('lets a plain number become a plural', () => {
    expect(check('and {n} more', 'ja veel {n, plural, one {#} other {#}}')).toEqual([])
  })

  it('catches a plural without a CLDR category of the language', () => {
    expect(check('{n, plural, one {# day} other {# days}}', '{n, plural, other {# päeva}}')).toEqual(['plural'])
  })

  it('catches a glossary term without its approved translation', () => {
    expect(check('Moved to the trash.', 'Viidi prügikasti.')).toEqual([])
    expect(check('Moved to the trash.', 'Viidi prügikorvi.')).toEqual(['glossary'])
    expect(check('2 folders', '2 kataloogi')).toEqual(['glossary'])
  })

  it('catches a kept word that was translated', () => {
    expect(check('Open it in Finder.', 'Ava see Finderis.')).toEqual([])
    expect(check('Open it in Finder.', 'Ava see failihalduris.')).toEqual(['keep'])
  })

  it('catches untranslated text, unless it is the same on purpose', () => {
    expect(check('Video player', 'Video player')).toContain('untranslated')
    expect(check('Video player', 'Video player', { sameAsSource: ['x.y'] })).toEqual([])
    expect(check('Claude Code', 'Claude Code')).toEqual([])
    expect(check('Copy the key now.', 'Kopeeri the võti now.')).toEqual(['untranslated'])
  })

  it('catches a missing status, an unknown state and a translation of an older English text', () => {
    expect(check('Done', 'Valmis', { status: {} })).toEqual(['status'])
    expect(check('Done', 'Valmis', { status: { 'x.y': `human:${sourceHash('Done')}` } })).toEqual(['status'])
    expect(check('Done', 'Valmis', { status: { 'x.y': `machine:${sourceHash('Finished')}` } })).toEqual(['status'])
    expect(check('Done', 'Valmis', { status: { 'x.y': `agent-reviewed:${sourceHash('Done')}` } })).toEqual([])
  })

  it('keeps the product "Ruum" and the space "ruum" apart in Estonian (decision 0005 B-1)', () => {
    // The product with a capital letter, where the English names it.
    expect(check('Sign in to Ruum', 'Logi Ruumi sisse')).toEqual([])
    expect(check("You're invited to a space in Ruum", 'Sind kutsutakse Ruumis ühte ruumi')).toEqual([])
    // A space with a capital letter: the English has no product name.
    expect(check('A space needs at least one owner.', 'Ruumil peab olema vähemalt üks omanik.')).toEqual(['ruum'])
    expect(check('Invite them under Space settings.', 'Kutsu ta jaotises „Ruumi seaded“.')).toEqual(['ruum'])
    expect(check('{hasSpace, select, yes {{space}} other {This space}} is full.', '{hasSpace, select, yes {Ruum {space}} other {See ruum}} on täis.')).toEqual(['ruum'])
    // More capital words than the English has product names.
    expect(check('Ruum is ready.', 'Ruum on valmis. Ruumi nimi on muudetud.')).toEqual(['ruum'])
    // A sentence does not start with the common word, also not after a full stop or in a branch.
    expect(check('Saved. The space is ready.', 'Salvestatud. ruum on valmis.')).toEqual(['ruum'])
    expect(check('{n, plural, one {# space} other {# spaces}}', '{n, plural, one {ruum #} other {ruumid #}}')).toEqual(['ruum', 'ruum'])
    expect(check('Every space needs an owner.', 'Igal ruumil peab olema omanik.')).toEqual([])
    // An empty branch does not leave the next word at the start.
    expect(check('{n, plural, =0 {} one {# member} other {# members}} of {space}', '{n, plural, =0 {} one {# liige} other {# liiget}} ruumis {space}')).toEqual([])
    // A compound or a word in the middle is fine.
    expect(check('Your team space is ready.', 'Sinu meeskonnaruum on valmis.')).toEqual([])
    // Other languages are not checked.
    const source = { 'x.y': 'A space needs an owner.' }
    const catalogue = { '@metadata': { status: { 'x.y': `machine:${sourceHash(source['x.y'])}` } }, messages: { 'x.y': 'Ruumil peab olema omanik.' } }
    expect(checkCatalogue({ tag: 'fi', source, catalogue, glossary: { terms: [], keep: [] } }).errors).toEqual([])
  })

  it('warns when the product and the space stand side by side in Estonian', () => {
    const source = { 'x.y': 'Ruum spaces' }
    const catalogue = { '@metadata': { status: { 'x.y': `machine:${sourceHash(source['x.y'])}` } }, messages: { 'x.y': 'Ruumi ruumid' } }
    const { errors, warnings } = checkCatalogue({ tag: 'et', source, catalogue, glossary })
    expect(errors).toEqual([])
    expect(warnings.map((w) => w.rule)).toEqual(['ruum'])
  })

  it('lists the messages to translate again', () => {
    const source = { a: 'One', b: 'Two', c: 'Three' }
    const catalogue = { '@metadata': { status: { a: `machine:${sourceHash('One')}`, b: `machine:${sourceHash('2')}` } }, messages: { a: 'Üks', b: 'Kaks' } }
    expect(changedKeys(source, catalogue)).toEqual(['b', 'c'])
  })
})
