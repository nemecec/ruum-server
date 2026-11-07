// The pseudo-locale (design 12.4): each letter of a message gets an accent, each vowel is doubled
// (English text grows by about 40%), and the message is put in brackets. Arguments, plural and
// select keys and tags stay as they are, so the message still formats. Text that the catalogue
// does not hold stays plain ASCII and is easy to find on the screen.
import {
  isLiteralElement,
  isPluralElement,
  isSelectElement,
  isTagElement,
  parse,
  TYPE,
  type MessageFormatElement,
} from '@formatjs/icu-messageformat-parser'

const ACCENTED: Record<string, string> = {
  a: 'å', b: 'ƀ', c: 'ç', d: 'ð', e: 'é', f: 'ƒ', g: 'ĝ', h: 'ĥ', i: 'î', j: 'ĵ', k: 'ķ', l: 'ļ', m: 'ɱ',
  n: 'ñ', o: 'ö', p: 'þ', q: 'ǫ', r: 'ŕ', s: 'š', t: 'ţ', u: 'û', v: 'ṽ', w: 'ŵ', x: 'ẋ', y: 'ý', z: 'ž',
  A: 'Å', B: 'Ɓ', C: 'Ç', D: 'Ð', E: 'É', F: 'Ƒ', G: 'Ĝ', H: 'Ĥ', I: 'Î', J: 'Ĵ', K: 'Ķ', L: 'Ļ', M: 'Ṁ',
  N: 'Ñ', O: 'Ö', P: 'Þ', Q: 'Ǫ', R: 'Ŕ', S: 'Š', T: 'Ţ', U: 'Û', V: 'Ṽ', W: 'Ŵ', X: 'Ẋ', Y: 'Ý', Z: 'Ž',
}
const VOWELS = new Set('aeiouyAEIOUY')

/** The open and close marks around every pseudo message. */
export const PSEUDO_OPEN = '['
export const PSEUDO_CLOSE = ']'

/** Accented, longer text (no brackets). */
export function pseudoText(text: string): string {
  let out = ''
  for (const ch of text) {
    const a = ACCENTED[ch]
    out += a ? (VOWELS.has(ch) ? a + a : a) : ch
  }
  return out
}

function walk(elements: MessageFormatElement[]): MessageFormatElement[] {
  return elements.map((el) => {
    if (isLiteralElement(el)) return { ...el, value: pseudoText(el.value) }
    if (isTagElement(el)) return { ...el, children: walk(el.children) }
    if (isPluralElement(el) || isSelectElement(el)) {
      const options: typeof el.options = {}
      for (const [k, o] of Object.entries(el.options)) options[k] = { ...o, value: walk(o.value) }
      return { ...el, options }
    }
    return el
  })
}

/** One ICU message in the pseudo-locale, as a parsed message for react-intl. */
export function pseudoMessage(message: string): MessageFormatElement[] {
  const lit = (value: string): MessageFormatElement => ({ type: TYPE.literal, value })
  return [lit(PSEUDO_OPEN), ...walk(parse(message)), lit(PSEUDO_CLOSE)]
}

/** A whole catalogue in the pseudo-locale. */
export function pseudoMessages(messages: Record<string, string>): Record<string, MessageFormatElement[]> {
  const out: Record<string, MessageFormatElement[]> = {}
  for (const [k, m] of Object.entries(messages)) out[k] = pseudoMessage(m)
  return out
}

/** A message without arguments as the pseudo-locale shows it (for tests that find text). */
export function pseudo(text: string): string {
  return PSEUDO_OPEN + pseudoText(text) + PSEUDO_CLOSE
}
