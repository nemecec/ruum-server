import { useCallback, useEffect, useRef, useState } from 'react'

export type ApiState<T> = {
  data: T | undefined
  error: unknown
  loading: boolean
  /** Loads the data again. */
  reload: () => void
}

/**
 * Loads data with `load` when the component mounts and when `deps` change.
 * A small helper instead of a query library; the UI is small (design section 12.3).
 */
export function useApi<T>(load: () => Promise<T>, deps: readonly unknown[]): ApiState<T> {
  const [data, setData] = useState<T | undefined>(undefined)
  const [error, setError] = useState<unknown>(undefined)
  const [loading, setLoading] = useState(true)
  const [tick, setTick] = useState(0)
  const loadRef = useRef(load)
  loadRef.current = load

  useEffect(() => {
    let active = true
    setLoading(true)
    loadRef
      .current()
      .then((d) => {
        if (!active) return
        setData(d)
        setError(undefined)
      })
      .catch((e: unknown) => {
        if (active) setError(e)
      })
      .finally(() => {
        if (active) setLoading(false)
      })
    return () => {
      active = false
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [...deps, tick])

  const reload = useCallback(() => setTick((t) => t + 1), [])
  return { data, error, loading, reload }
}
