// The component catalogue as a test (design 12.5): every story of every component renders, and
// axe finds no WCAG 2.2 A/AA violation in it. jsdom has no layout, so axe cannot check contrast
// here; the token pairs are checked by docs/ux/tools/contrast.mjs and the screens by
// e2e/a11y.e2e.ts.
import { act, render } from '@testing-library/react'
import axe from 'axe-core'
import type { ComponentType } from 'react'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'

// axe probes a canvas for icon fonts; jsdom has none.
HTMLCanvasElement.prototype.getContext = (() => null) as typeof HTMLCanvasElement.prototype.getContext

const modules = import.meta.glob<Record<string, unknown>>('./**/*.stories.tsx', { eager: true })

const stories = Object.entries(modules).flatMap(([file, mod]) =>
  Object.entries(mod)
    .filter(([name, value]) => name !== 'default' && typeof value === 'function')
    .map(([name, value]) => ({ file, name, Story: value as ComponentType })),
)

describe('component catalogue', () => {
  it('has stories for every component folder', () => {
    const folders = new Set(Object.keys(modules).map((f) => f.split('/')[1]))
    expect(folders.size).toBeGreaterThanOrEqual(16)
  })

  it.each(stories.map((s) => [`${s.file.replace('./', '')} ${s.name}`, s] as const))(
    '%s renders without axe violations',
    async (_, { Story }) => {
      const { container } = render(
        <MemoryRouter>
          <main>
            <Story />
          </main>
        </MemoryRouter>,
      )
      await act(async () => {})
      const result = await axe.run(container, {
        runOnly: { type: 'tag', values: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'] },
        rules: { 'color-contrast': { enabled: false }, region: { enabled: false } },
      })
      expect(result.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target.join(' ')).join(', ')}`)).toEqual([])
    },
  )
})
