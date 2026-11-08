// Context for translators (design 12.4.1): every message of the source catalogue has a
// description, a tone and a length budget in src/locales/context/en.json, and the English text
// fits its own budget. docs/ux/i18n.md, "Context for translators", says why it is a sidecar file.
import { parse, type MessageFormatElement } from '@formatjs/icu-messageformat-parser'
import { describe, expect, it } from 'vitest'
import en from '../locales/en.json'
import context from '../locales/context/en.json'

type Entry = { description: string; tone: string; maxLength: number }
const entries = context.messages as Record<string, Entry>
const source = en.messages as Record<string, string>

/**
 * The longest visible text of a message, in characters: each plural or select counts its longest
 * branch, a tag counts its children, and an argument counts 8 (a typical short value).
 */
function visibleLength(elements: MessageFormatElement[]): number {
  let n = 0
  for (const el of elements) {
    if (el.type === 0) n += el.value.length // literal
    else if (el.type === 7) n += 1 // `#` in a plural
    else if ('options' in el) n += Math.max(...Object.values(el.options).map((o) => visibleLength(o.value)))
    else if ('children' in el) n += visibleLength(el.children)
    else n += 8
  }
  return n
}

describe('context for translators', () => {
  it('has an entry for every message of en.json, and none for a message that is gone', () => {
    expect(Object.keys(entries).filter((k) => !(k in source)), 'entries without a message').toEqual([])
    expect(Object.keys(source).filter((k) => !(k in entries)), 'messages without an entry').toEqual([])
  })

  it('gives each entry a description, a tone of the voice and a length budget', () => {
    for (const [key, e] of Object.entries(entries)) {
      expect(e.description.trim().length, key).toBeGreaterThan(20)
      expect(['playful', 'neutral', 'calm'], key).toContain(e.tone)
      expect(Number.isInteger(e.maxLength) && e.maxLength > 0, key).toBe(true)
    }
  })

  it('keeps every English message within its own length budget', () => {
    const over = Object.entries(entries)
      .map(([key, e]) => ({ key, length: visibleLength(parse(source[key])), max: e.maxLength }))
      .filter((x) => x.length > x.max)
    expect(over).toEqual([])
  })
})
