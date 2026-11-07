// The message catalogues (design 12.4): src/locales/<tag>.json, one ICU message per key. Keys
// name the screen and the meaning (`files.empty.title`). English is bundled; the others load when
// they are needed. A message that a translation lacks falls back to English.
import type { MessageFormatElement } from '@formatjs/icu-messageformat-parser'
import en from '../locales/en.json'
import { PSEUDO_LOCALE, SOURCE_LANGUAGE } from './config'
import { pseudoMessages } from './pseudo'

/** The metadata of a catalogue. A machine translation stays marked until a person checks it. */
export type CatalogueMeta = {
  locale: string
  source?: string
  /** `source`, `machine` (translated by an agent) or `human` (checked by a person). */
  translation: 'source' | 'machine' | 'human'
  note?: string
}

export type Catalogue = { '@metadata': CatalogueMeta; messages: Record<string, string> }

/** Every key of the source catalogue. */
export type MessageKey = keyof typeof en.messages

export type Messages = Record<string, string> | Record<string, MessageFormatElement[]>

/** The English messages: the source and the fallback. */
export const sourceMessages: Record<string, string> = en.messages

const files = import.meta.glob<Catalogue>(['../locales/*.json', '!../locales/en.json'], { import: 'default' })

/** The messages of a catalogue (`en`, `et`, the pseudo-locale). */
export async function loadMessages(language: string): Promise<Messages> {
  if (language === SOURCE_LANGUAGE) return sourceMessages
  if (language === PSEUDO_LOCALE) return pseudoMessages(sourceMessages)
  const load = files[`../locales/${language}.json`]
  if (!load) return sourceMessages
  const catalogue = await load()
  return { ...sourceMessages, ...catalogue.messages }
}
