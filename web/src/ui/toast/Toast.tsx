import { useCallback, useEffect, useMemo, useRef, useState, type HTMLAttributes, type ReactNode } from 'react'
import { errorText } from '../../api/errors'
import { Button, IconButton } from '../button/Button'
import { cx } from '../cx'
import { Icon } from '../icon/Icon'
import { ToastContext, type ToastOptions } from './useToast'
import './toast.css'
import { t } from '../../i18n'

type ToastData = { id: number; text: string; options: ToastOptions; error?: boolean }

let nextId = 1

/**
 * The toasts of the app (design-system 4.13): at most 3, the oldest leaves first. Success toasts
 * stay 6 s, toasts with an action 10 s; hover or focus pauses the timer.
 */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastData[]>([])
  const remove = useCallback((id: number) => setToasts((list) => list.filter((t) => t.id !== id)), [])
  const toast = useCallback((text: string, options: ToastOptions = {}) => {
    setToasts((list) => [...list.slice(-2), { id: nextId++, text, options }])
  }, [])
  const update = useCallback(
    (id: number, patch: Partial<ToastData>) => setToasts((list) => list.map((t) => (t.id === id ? { ...t, ...patch } : t))),
    [],
  )
  const api = useMemo(() => ({ toast }), [toast])
  return (
    <ToastContext.Provider value={api}>
      {children}
      <ToastRegion>
        {toasts.map((t) => (
          <ToastItem key={t.id} toast={t} onDone={() => remove(t.id)} onUpdate={(patch) => update(t.id, patch)} />
        ))}
      </ToastRegion>
    </ToastContext.Provider>
  )
}

/** The fixed region of the toasts. */
export function ToastRegion({ children }: { children: ReactNode }) {
  return (
    <section className="ui-toasts" aria-label={t('ui.toasts')}>
      {children}
    </section>
  )
}

/** One toast as it looks, without timers (for the catalogue). */
export function ToastView(
  props: { text: string; error?: boolean; children?: ReactNode; onDismiss: () => void } & Omit<
    HTMLAttributes<HTMLDivElement>,
    'className' | 'style'
  >,
) {
  const { text, error, children, onDismiss, ...rest } = props
  return (
    <div className={cx('ui-toast', error && 'ui-toast-error')} {...rest}>
      <Icon name={error ? 'error' : 'success'} />
      <p role="status">{text}</p>
      {children}
      <IconButton icon="close" size="sm" label={t('ui.toast.dismiss')} onClick={onDismiss} />
    </div>
  )
}

function ToastItem({ toast, onDone, onUpdate }: { toast: ToastData; onDone: () => void; onUpdate: (patch: Partial<ToastData>) => void }) {
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
    <ToastView
      text={toast.text}
      error={toast.error}
      onDismiss={onDone}
      onMouseEnter={() => clearTimeout(timer.current)}
      onMouseLeave={start}
      onFocus={() => clearTimeout(timer.current)}
      onBlur={start}
    >
      {toast.options.undo && (
        <Button
          size="sm"
          disabled={busy}
          busy={busy}
          onClick={async () => {
            setBusy(true)
            try {
              const text = await toast.options.undo!()
              onUpdate({ text: text || t('ui.toast.undone'), options: {} })
            } catch (e) {
              onUpdate({ text: errorText(e), options: {}, error: true })
            }
          }}
        >
          {t('ui.toast.undo')}
        </Button>
      )}
      {toast.options.action && (
        <Button
          size="sm"
          onClick={() => {
            toast.options.action!.run()
            onDone()
          }}
        >
          {toast.options.action.label}
        </Button>
      )}
    </ToastView>
  )
}
