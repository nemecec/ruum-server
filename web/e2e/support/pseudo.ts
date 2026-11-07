// The pseudo-locale in the browser tests (design 12.4): the names of buttons and dialogs as the
// pseudo-locale writes them, and a check that no text of the source catalogue is left in English.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import type { Page } from '@playwright/test'
import { pseudo, pseudoText } from '../../src/i18n/pseudo.ts'

export { pseudo }

/** Text in one language or the other: `words('Share')` is "Share", or its pseudo form. */
export type Words = { exact: (text: string) => string; prefix: (text: string) => RegExp }

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')

export const english: Words = { exact: (t) => t, prefix: (t) => new RegExp(`^${escape(t)}`) }
export const pseudoWords: Words = { exact: pseudo, prefix: (t) => new RegExp(`^${escape(`[${pseudoText(t)}`)}`) }

const catalogue = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'src', 'locales', 'en.json')

/**
 * Product names and example values of the catalogue, which test data may also hold ("Claude
 * Desktop" is an agent of the mock API, "laptop" the name of a key).
 */
const sameAsData = /(^|\.)placeholder$|^connect\.(claudeDesktop|claudeCode|chatgpt)$|^account\.provider\.|^app\.name$/

/** The messages of the source catalogue that have no arguments: the text a screen shows as is. */
function sourceTexts(): string[] {
  const { messages } = JSON.parse(fs.readFileSync(catalogue, 'utf8')) as { messages: Record<string, string> }
  const texts = Object.entries(messages).filter(([k, m]) => !sameAsData.test(k) && !/[{<]/.test(m) && /[A-Za-z]{3}/.test(m))
  return [...new Set(texts.map(([, m]) => m))]
}

/**
 * Visible texts and accessible names on the page that are English messages of the catalogue: in
 * the pseudo-locale every message is accented, so such a text did not come through the catalogue.
 */
export async function untranslated(page: Page): Promise<string[]> {
  const known = sourceTexts()
  return page.evaluate((known) => {
    const set = new Set(known)
    const found: string[] = []
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT)
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const text = (n.textContent ?? '').trim()
      const el = n.parentElement
      if (text && set.has(text) && el && el.checkVisibility()) found.push(text)
    }
    for (const el of Array.from(document.querySelectorAll('[aria-label],[placeholder],[title]')))
      for (const a of ['aria-label', 'placeholder', 'title']) {
        const v = el.getAttribute(a)?.trim()
        if (v && set.has(v)) found.push(`${a}="${v}"`)
      }
    return found
  }, known)
}
