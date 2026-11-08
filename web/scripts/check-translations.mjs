// The automatic checks of the translation pipeline (design 12.4.1, step 3). For every translation
// catalogue in src/locales/<tag>.json (not en.json, the source), each message must:
//
// 1. icu          be valid ICU MessageFormat;
// 2. keys         exist for every key of the source, and only for those;
// 3. arguments    use the same placeholders, select keys and tags as the English message (a plain
//                 number may become a plural);
// 4. plural       have every CLDR plural category of the language in each plural;
// 5. glossary     use the approved term: when the English text uses a term of
//                 docs/ux/glossary.md that has stems in the column of the language ("kaust
//                 (kaust-)"), the translation contains one of the stems;
// 6. keep         keep each word of the do-not-translate list of the glossary (products,
//                 protocols, SQL words) that the English text has;
// 7. untranslated not be the English text (unless `@metadata.sameAsSource` lists the key) and
//                 have no English function word ("the", "your", ...) outside the placeholders;
// 8. status       have a status in `@metadata.status`: "<state>:<hash>", where the state is
//                 `machine` (an agent translated it) or `agent-reviewed` (a reviewer agent checked
//                 it) and the hash is the start of the SHA-256 of the English message it was made
//                 from. A different hash means the English changed after the translation: the
//                 message must be translated again (pipeline step 1).
//
// Warnings (they do not fail): a translation longer than the length budget of its message
// (`maxLength` in src/locales/context/en.json; the visual tests check the real fit).
//
// Run:
//   node scripts/check-translations.mjs              check (part of `npm run lint`); exit code 1 on errors
//   node scripts/check-translations.mjs --verbose    also list the warnings
//   node scripts/check-translations.mjs --changed    list the messages to translate again, per language
//   node scripts/check-translations.mjs --mark <state> <tag> <key>... | --stale | --all
//                                                    set the status of messages to <state> with the
//                                                    hash of today's English text (after translating
//                                                    or reviewing them)
import crypto from 'node:crypto'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { parse } from '@formatjs/icu-messageformat-parser'

const web = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
const locales = path.join(web, 'src', 'locales')
const glossaryFile = path.join(web, '..', 'docs', 'ux', 'glossary.md')

export const STATES = ['machine', 'agent-reviewed']

/** The product name: never translated (docs/ux/glossary.md, "Do not translate"). */
const PRODUCT = 'Ruum'

/** English words that never belong in a translation, outside placeholders and kept names. */
const ENGLISH = new Set([
  'the', 'and', 'you', 'your', "you're", 'with', 'this', 'that', 'from', 'for', 'of', 'is', 'are', 'not',
  "can't", "don't", "isn't", "doesn't", "didn't", 'here', 'there', 'have', 'has', 'will', 'what', 'who',
])

// Element types of @formatjs/icu-messageformat-parser.
const LITERAL = 0, ARGUMENT = 1, NUMBER = 2, DATE = 3, TIME = 4, SELECT = 5, PLURAL = 6, POUND = 7, TAG = 8

/** The first 8 hex digits of the SHA-256 of a source message: the `<hash>` of a status. */
export function sourceHash(message) {
  return crypto.createHash('sha256').update(message).digest('hex').slice(0, 8)
}

/** The text a reader sees, from every branch, without the placeholders. */
export function literalText(elements) {
  const out = []
  for (const el of elements) {
    if (el.type === LITERAL) out.push(el.value)
    else if (el.type === TAG) out.push(literalText(el.children))
    else if (el.type === SELECT || el.type === PLURAL) for (const o of Object.values(el.options)) out.push(literalText(o.value))
    else out.push(' ')
  }
  return out.join(' ').replace(/\s+/g, ' ').trim()
}

/** The placeholders of a message by name and kind, and the option keys of each select. */
export function argsOf(elements, args = new Map(), selects = new Map()) {
  for (const el of elements) {
    if (el.type === ARGUMENT) args.set(el.value, args.get(el.value) ?? 'plain')
    // A plain number may become a plural in a translation ("{n} files" -> "{n, plural, ...}").
    else if (el.type === NUMBER || el.type === PLURAL) args.set(el.value, 'plain')
    else if (el.type === DATE || el.type === TIME) args.set(el.value, 'date')
    else if (el.type === TAG) {
      args.set(el.value, 'tag')
      argsOf(el.children, args, selects)
    }
    if (el.type === SELECT) {
      args.set(el.value, 'select')
      selects.set(el.value, new Set(Object.keys(el.options)))
    }
    if (el.type === SELECT || el.type === PLURAL) for (const o of Object.values(el.options)) argsOf(o.value, args, selects)
  }
  return { args, selects }
}

/** The option keys of every plural of a message. */
export function pluralForms(elements, out = []) {
  for (const el of elements) {
    if (el.type === PLURAL) out.push(Object.keys(el.options))
    if (el.type === SELECT || el.type === PLURAL) for (const o of Object.values(el.options)) pluralForms(o.value, out)
    if (el.type === TAG) pluralForms(el.children, out)
  }
  return out
}

/**
 * The longest visible text of a message, in characters: each plural or select counts its longest
 * branch, a tag counts its children, and a placeholder counts 8 (as src/i18n/context.test.ts).
 */
export function visibleLength(elements) {
  let n = 0
  for (const el of elements) {
    if (el.type === LITERAL) n += el.value.length
    else if (el.type === POUND) n += 1
    else if (el.type === SELECT || el.type === PLURAL) n += Math.max(...Object.values(el.options).map((o) => visibleLength(o.value)))
    else if (el.type === TAG) n += visibleLength(el.children)
    else n += 8
  }
  return n
}

/** The cells of a Markdown table row. */
function cells(line) {
  return line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map((c) => c.trim())
}

/** Alternatives of a glossary cell: "Trash; Move to trash", "Pause / Resume". */
function alternatives(cell) {
  // Parenthesised notes may hold separators; take them out first.
  const notes = []
  const bare = cell.replace(/\([^)]*\)/g, (m) => `\uE000${notes.push(m) - 1}\uE000`)
  return bare
    .split(/ \/ |; /)
    .map((part) => part.replace(/\uE000(\d+)\uE000/g, (_, i) => notes[Number(i)]).trim())
    .filter(Boolean)
}

/**
 * The glossary for one language: `terms` (an English term and the stems of its approved
 * translation; only terms whose cell lists stems) and `keep` (the words of the do-not-translate
 * list that stay as they are: other products, protocols and formats, database words).
 */
export function parseGlossary(markdown, language) {
  const terms = []
  const keep = new Set()
  const lines = markdown.split('\n')
  let header = null
  let section = ''
  for (const line of lines) {
    if (line.startsWith('## ')) section = line.slice(3).trim()
    if (!line.trim().startsWith('|')) {
      header = null
      continue
    }
    const row = cells(line)
    if (!header) {
      header = row
      continue
    }
    if (row.every((c) => /^-+$/.test(c))) continue
    if (section === 'Do not translate') {
      const kind = row[header.indexOf('Kind')]
      if (!['Other products', 'Protocols and formats', 'Database names and words'].includes(kind)) continue
      for (const item of row[header.indexOf('Words')].replace(/\*\*/g, '').split(/[,()]/)) {
        const word = item.trim()
        // Keep names and acronyms, not descriptions ("SQL keywords") or lower-case extension names.
        if (word && word !== '...' && !word.split(/\s+/).some((w) => /^[a-z]+$/.test(w))) keep.add(word)
      }
      continue
    }
    const t = header.indexOf('Term')
    const l = header.indexOf(language)
    if (t < 0 || l < 0) continue
    const english = alternatives(row[t])
    const translated = alternatives(row[l] ?? '')
    if (translated.length !== 1 && translated.length !== english.length)
      throw new Error(`glossary: "${row[t]}" has ${english.length} terms but ${language} "${row[l]}" has ${translated.length}`)
    english.forEach((term, i) => {
      if (term.includes('*')) return // a pattern with a value, such as "Everyone in *Space*"
      const cell = translated.length === 1 ? translated[0] : translated[i]
      const stems = [...cell.matchAll(/\(([^)]*)\)/g)]
        .flatMap((m) => m[1].split(','))
        .map((s) => s.trim())
        .filter((s) => s.endsWith('-'))
        .map((s) => s.slice(0, -1).toLowerCase())
      const word = term.replace(/\([^)]*\)/g, '').trim().toLowerCase()
      if (stems.length && word) terms.push({ term: word, stems })
    })
  }
  return { terms, keep: [...keep] }
}

const escape = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

/** Does an English text use the term (also as a plural)? */
function usesTerm(text, term) {
  return new RegExp(`(^|[^\\p{L}])${escape(term)}(s|es)?($|[^\\p{L}])`, 'iu').test(text)
}

/** Does a text have the word as it is (case-sensitive, not inside a longer word before it)? */
function hasWord(text, word) {
  return new RegExp(`(^|[^\\p{L}])${escape(word)}($|[^\\p{L}])`, 'u').test(text)
}

/**
 * Checks one translation catalogue against the source. Returns the errors and the warnings, each
 * `{ key, rule, message }`.
 */
export function checkCatalogue({ tag, source, catalogue, glossary, context = {} }) {
  const errors = []
  const warnings = []
  const fail = (key, rule, message) => errors.push({ key, rule, message })
  const messages = catalogue.messages ?? {}
  const meta = catalogue['@metadata'] ?? {}
  const status = meta.status ?? {}
  const same = new Set(meta.sameAsSource ?? [])
  const needed = new Intl.PluralRules(tag).resolvedOptions().pluralCategories

  for (const key of Object.keys(messages)) if (!(key in source)) fail(key, 'keys', 'not in the source catalogue')
  for (const key of Object.keys(status)) if (!(key in source)) fail(key, 'status', 'status of a message that is not in the source')
  for (const key of same) if (!(key in source)) fail(key, 'untranslated', 'sameAsSource lists a message that is not in the source')

  for (const [key, english] of Object.entries(source)) {
    const message = messages[key]
    if (message === undefined) {
      fail(key, 'keys', 'missing')
      continue
    }
    let have
    try {
      have = parse(message)
    } catch (e) {
      fail(key, 'icu', `not valid ICU: ${e.message}`)
      continue
    }
    const want = parse(english)

    // 3. arguments
    const a = argsOf(want)
    const b = argsOf(have)
    const show = (m) => [...m].map(([n, k]) => `${n} (${k})`).sort().join(', ') || 'none'
    if (show(a.args) !== show(b.args)) fail(key, 'arguments', `placeholders ${show(b.args)}, the source has ${show(a.args)}`)
    for (const [name, keys] of a.selects) {
      const got = [...(b.selects.get(name) ?? [])].sort().join(' ')
      if (got !== [...keys].sort().join(' ')) fail(key, 'arguments', `select ${name} has keys "${got}", the source has "${[...keys].sort().join(' ')}"`)
    }

    // 4. plural
    for (const forms of pluralForms(have))
      for (const form of needed) if (!forms.includes(form)) fail(key, 'plural', `a plural has no "${form}" form (${tag} needs ${needed.join(', ')})`)

    const en = literalText(want)
    const text = literalText(have)
    const lower = text.toLowerCase()

    // 5. glossary
    for (const { term, stems } of glossary.terms)
      if (usesTerm(en, term) && !stems.some((s) => lower.includes(s)))
        fail(key, 'glossary', `the English uses "${term}": use the approved term (${stems.map((s) => `${s}-`).join(', ')})`)

    // 6. keep
    for (const word of glossary.keep)
      if (hasWord(en, word) && !text.includes(word)) fail(key, 'keep', `"${word}" stays as it is (do-not-translate list)`)

    // 7. untranslated
    if (!same.has(key)) {
      // The product name and the kept words may stay; anything else of three letters or more is English.
      const leftover = [...glossary.keep, PRODUCT]
        .sort((x, y) => y.length - x.length)
        .reduce((t, k) => t.replace(new RegExp(escape(k), 'g'), ' '), en)
        .split(/\s+/)
        .filter((w) => /\p{L}{3,}/u.test(w))
      if (message === english && leftover.length) fail(key, 'untranslated', 'the same as the English text')
      else {
        const words = lower.match(/[\p{L}']+/gu) ?? []
        const found = words.filter((w) => ENGLISH.has(w))
        if (found.length) fail(key, 'untranslated', `English words: ${[...new Set(found)].join(', ')}`)
      }
    }

    // 8. status
    const s = status[key]
    const [state, hash] = typeof s === 'string' ? s.split(':') : []
    if (!s) fail(key, 'status', 'no status in @metadata.status')
    else if (!STATES.includes(state)) fail(key, 'status', `status "${s}": the state is ${STATES.join(' or ')}`)
    else if (hash !== sourceHash(english)) fail(key, 'status', 'the English changed after this translation: translate it again, then mark it')

    // Warning: length budget
    const max = context[key]?.maxLength
    if (max && visibleLength(have) > max) warnings.push({ key, rule: 'length', message: `${visibleLength(have)} characters, the budget is ${max}` })
  }
  return { errors, warnings }
}

/** The keys whose translation is missing or older than the English text. */
export function changedKeys(source, catalogue) {
  const status = catalogue['@metadata']?.status ?? {}
  return Object.keys(source).filter((key) => {
    const [, hash] = (status[key] ?? '').split(':')
    return !(key in (catalogue.messages ?? {})) || hash !== sourceHash(source[key])
  })
}

const read = (file) => JSON.parse(fs.readFileSync(file, 'utf8'))

/** The translation catalogues: tag -> file. */
function catalogueFiles() {
  return fs
    .readdirSync(locales)
    .filter((f) => f.endsWith('.json') && f !== 'en.json')
    .map((f) => [f.slice(0, -5), path.join(locales, f)])
}

function languageName(tag) {
  return new Intl.DisplayNames(['en'], { type: 'language' }).of(tag)
}

function main(argv) {
  const source = read(path.join(locales, 'en.json')).messages
  const context = read(path.join(locales, 'context', 'en.json')).messages
  const markdown = fs.readFileSync(glossaryFile, 'utf8')

  if (argv[0] === '--mark') {
    const [, state, tag, ...rest] = argv
    if (!STATES.includes(state) || !tag || !rest.length) {
      console.error('usage: check-translations.mjs --mark <machine|agent-reviewed> <tag> <key>... | --stale | --all')
      return 2
    }
    const file = path.join(locales, `${tag}.json`)
    const catalogue = read(file)
    const keys = rest[0] === '--all' ? Object.keys(source) : rest[0] === '--stale' ? changedKeys(source, catalogue) : rest
    const unknown = keys.filter((k) => !(k in source))
    if (unknown.length) {
      console.error(`not in the source: ${unknown.join(', ')}`)
      return 2
    }
    const status = { ...(catalogue['@metadata'].status ?? {}) }
    for (const key of keys) status[key] = `${state}:${sourceHash(source[key])}`
    catalogue['@metadata'].status = Object.fromEntries(Object.keys(source).filter((k) => k in status).map((k) => [k, status[k]]))
    fs.writeFileSync(file, `${JSON.stringify(catalogue, null, 2)}\n`)
    console.log(`${tag}: marked ${keys.length} messages ${state}`)
    return 0
  }

  if (argv[0] === '--changed') {
    for (const [tag, file] of catalogueFiles()) {
      const keys = changedKeys(source, read(file))
      console.log(`${tag}: ${keys.length} messages to translate${keys.length ? ':' : ''}`)
      for (const key of keys) console.log(`  ${key}`)
    }
    return 0
  }

  let failed = 0
  for (const [tag, file] of catalogueFiles()) {
    const glossary = parseGlossary(markdown, languageName(tag))
    const { errors, warnings } = checkCatalogue({ tag, source, catalogue: read(file), glossary, context })
    for (const e of errors) console.error(`${tag} ${e.key}: [${e.rule}] ${e.message}`)
    if (argv.includes('--verbose')) for (const w of warnings) console.warn(`${tag} ${w.key}: [${w.rule}, warning] ${w.message}`)
    const counts = Object.entries(Object.groupBy(read(file)['@metadata'].status ? Object.values(read(file)['@metadata'].status) : [], (s) => s.split(':')[0]))
      .map(([state, list]) => `${list.length} ${state}`)
      .join(', ')
    console.log(
      `${tag}: ${errors.length} errors, ${warnings.length} warnings (length budget${argv.includes('--verbose') ? '' : '; --verbose lists them'}); status: ${counts || 'none'}`,
    )
    failed += errors.length
  }
  return failed ? 1 : 0
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main(process.argv.slice(2))
