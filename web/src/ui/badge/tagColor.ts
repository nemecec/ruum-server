// The tag colours of design-system 2.1, and the mapping of the API's free `color` value to one of them.

/** The fixed tag colours (design-system 2.1). */
export const tagColors = ['spruce', 'moss', 'clay', 'brick', 'ochre', 'slate', 'plum', 'sand'] as const
export type TagColor = (typeof tagColors)[number]

/** The light values of the tag colours, to map the API's free `color` to the nearest one. */
const tagHex: Record<TagColor, string> = {
  spruce: '#1d5c57',
  moss: '#2b6439',
  clay: '#8a4718',
  brick: '#a3301c',
  ochre: '#7f5300',
  slate: '#4a5868',
  plum: '#6e3b5e',
  sand: '#8a7a55',
}

function rgb(hex: string): [number, number, number] | null {
  const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(hex.trim())
  if (!m) return null
  const h = m[1].length === 3 ? [...m[1]].map((c) => c + c).join('') : m[1]
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)) as [number, number, number]
}

/** The tag colour nearest to a free colour value (`#3a7`, `#33aa77`); none when it is not a colour. */
export function nearestTagColor(color: string | null | undefined): TagColor | null {
  const c = color ? rgb(color) : null
  if (!c) return null
  let best: TagColor = 'spruce'
  let bestDistance = Infinity
  for (const name of tagColors) {
    const t = rgb(tagHex[name])!
    const d = (t[0] - c[0]) ** 2 + (t[1] - c[1]) ** 2 + (t[2] - c[2]) ** 2
    if (d < bestDistance) {
      bestDistance = d
      best = name
    }
  }
  return best
}
