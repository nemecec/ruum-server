import { useState } from 'react'
import { useApi, type ApiState } from './useApi'

/** One page of a list operation (`next_cursor` is `null` on the last page). */
export type Page<T> = { items: T[]; next_cursor?: string | null }

export type Paged<T> = { items: T[]; nextCursor: string | null }

export type PagedState<T> = ApiState<Paged<T>> & {
  /** Loads the next page and appends it. */
  loadMore: () => void
  loadingMore: boolean
  moreError: unknown
}

/**
 * Loads the first page of a list with `load(undefined)`, and the next pages with `loadMore()`,
 * which follows `next_cursor`. `reload()` starts again at the first page.
 */
export function usePagedApi<T, X extends object = object>(
  load: (cursor: string | undefined) => Promise<Page<T> & X>,
  deps: readonly unknown[],
): PagedState<T> & { /** The first page as `load` answered it, with its other fields (for example a head). */ firstPage: (Page<T> & X) | undefined } {
  const first = useApi(() => load(undefined), deps)
  // The pages after the first one, for the first page they belong to (a reload drops them).
  const [extra, setExtra] = useState<{ base: Page<T>; items: T[]; next: string | null } | null>(null)
  const [loadingMore, setLoadingMore] = useState(false)
  const [moreError, setMoreError] = useState<unknown>()

  const current = extra && extra.base === first.data ? extra : null
  const data: Paged<T> | undefined = first.data
    ? {
        items: current ? [...first.data.items, ...current.items] : first.data.items,
        nextCursor: current ? current.next : (first.data.next_cursor ?? null),
      }
    : undefined

  function loadMore() {
    const base = first.data
    const cursor = data?.nextCursor
    if (!base || !cursor) return
    const prior = current?.items ?? []
    setLoadingMore(true)
    setMoreError(undefined)
    load(cursor)
      .then((p) => setExtra({ base, items: [...prior, ...p.items], next: p.next_cursor ?? null }))
      .catch(setMoreError)
      .finally(() => setLoadingMore(false))
  }

  return { ...first, data, firstPage: first.data, loadMore, loadingMore, moreError }
}
