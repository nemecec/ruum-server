import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { errorText } from '../api/errors'
import { ToastContext, type ToastOptions } from './useToast'

type Toast = { id: number; text: string; options: ToastOptions; error?: boolean }

let nextId = 1

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([])
  const remove = useCallback((id: number) => setToasts((list) => list.filter((t) => t.id !== id)), [])
  const toast = useCallback((text: string, options: ToastOptions = {}) => {
    setToasts((list) => [...list.slice(-2), { id: nextId++, text, options }])
  }, [])
  const update = useCallback((id: number, patch: Partial<Toast>) => setToasts((list) => list.map((t) => (t.id === id ? { ...t, ...patch } : t))), [])
  const api = useMemo(() => ({ toast }), [toast])
  return (
    <ToastContext.Provider value={api}>
      {children}
      <section className="toasts" aria-label="Notifications">
        {toasts.map((t) => (
          <ToastItem key={t.id} toast={t} onDone={() => remove(t.id)} onUpdate={(patch) => update(t.id, patch)} />
        ))}
      </section>
    </ToastContext.Provider>
  )
}

function ToastItem({ toast, onDone, onUpdate }: { toast: Toast; onDone: () => void; onUpdate: (patch: Partial<Toast>) => void }) {
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const [busy, setBusy] = useState(false)
  const hasAction = Boolean(toast.options.undo || toast.options.action)
  const start = useCallback(() => {
    clearTimeout(timer.current)
    timer.current = setTimeout(onDone, hasAction ? 10_000 : 6_000)
  }, [onDone, hasAction])
  useEffect(() => {
    start()
    return () => clearTimeout(timer.current)
  }, [start, toast.text])

  return (
    <div
      className={toast.error ? 'toast toast-error' : 'toast'}
      onMouseEnter={() => clearTimeout(timer.current)}
      onMouseLeave={start}
      onFocus={() => clearTimeout(timer.current)}
      onBlur={start}
    >
      <p role="status">{toast.text}</p>
      {toast.options.undo && (
        <button
          type="button"
          disabled={busy}
          onClick={async () => {
            setBusy(true)
            try {
              const text = await toast.options.undo!()
              onUpdate({ text: text || 'Undone.', options: {} })
            } catch (e) {
              onUpdate({ text: errorText(e), options: {}, error: true })
            }
          }}
        >
          Undo
        </button>
      )}
      {toast.options.action && (
        <button
          type="button"
          onClick={() => {
            toast.options.action!.run()
            onDone()
          }}
        >
          {toast.options.action.label}
        </button>
      )}
      <button type="button" className="icon" aria-label="Dismiss" onClick={onDone}>
        ×
      </button>
    </div>
  )
}
