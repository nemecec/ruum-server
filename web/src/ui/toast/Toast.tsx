import { useCallback, useEffect, useMemo, useRef, useState, type HTMLAttributes, type ReactNode, type Ref } from 'react'
import { errorText } from '../../api/errors'
import { Button, IconButton } from '../button/Button'
import { cx } from '../cx'
import { Icon } from '../icon/Icon'
import { ToastContext, type ToastOptions } from './useToast'
import './toast.css'
import { t } from '../../i18n'

type ToastData = { id: number; text: string; options: ToastOptions; error?: boolean }

let nextId = 1

/** How long a toast stays: a plain one 6 s, one with an action 30 s. An error stays until closed. */
const PLAIN_MS = 6_000
const ACTION_MS = 30_000

/**
 * The toasts of the app (design-system 4.13): at most 3, the oldest leaves first. Plain toasts stay
 * 6 s, toasts with an action 30 s; hover or focus pauses the timer. Error toasts stay until they
 * are closed. Alt+T moves the focus to the newest toast.
 *
 * Screen readers hear toasts through two live regions that exist from the start (one polite
 * `status`, one assertive for errors); a toast is added into the right one, so it is read. A region
 * that is inserted with its text already in it is often not read.
 */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<ToastData[]>([])
  const region = useRef<HTMLElement>(null)
  const remove = useCallback((id: number) => setToasts((list) => list.filter((t) => t.id !== id)), [])
  const toast = useCallback((text: string, options: ToastOptions = {}) => {
    setToasts((list) => [...list.slice(-2), { id: nextId++, text, options, error: options.error }])
  }, [])
  const update = useCallback(
    (id: number, patch: Partial<ToastData>) => setToasts((list) => list.map((t) => (t.id === id ? { ...t, ...patch } : t))),
    [],
  )
  const item = (x: ToastData) => <ToastItem key={x.id} toast={x} onDone={() => remove(x.id)} onUpdate={(patch) => update(x.id, patch)} />

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.code !== 'KeyT') return
      const all = region.current?.querySelectorAll<HTMLElement>('.ui-toast')
      const newest = all?.[all.length - 1]
      if (!newest) return
      e.preventDefault()
      newest.focus()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  const api = useMemo(() => ({ toast }), [toast])
  return (
    <ToastContext.Provider value={api}>
      {children}
      <ToastRegion regionRef={region}>
        <div className="ui-toast-live" role="status" aria-live="polite">
          {toasts.filter((x) => !x.error).map(item)}
        </div>
        {/* Assertive, as an alert, without the alert role: an empty alert would look like an error to tools that list alerts. */}
        <div className="ui-toast-live" aria-live="assertive">
          {toasts.filter((x) => x.error).map(item)}
        </div>
      </ToastRegion>
    </ToastContext.Provider>
  )
}

/** The fixed region of the toasts. */
export function ToastRegion({ children, regionRef }: { children: ReactNode; regionRef?: Ref<HTMLElement> }) {
  return (
    <section ref={regionRef} className="ui-toasts" aria-label={t('ui.toasts')}>
      {children}
    </section>
  )
}

/** One toast as it looks, without timers (for the catalogue). The provider puts it into a live region. */
export function ToastView(
  props: { text: string; error?: boolean; children?: ReactNode; onDismiss: () => void } & Omit<
    HTMLAttributes<HTMLDivElement>,
    'className' | 'style'
  >,
) {
  const { text, error, children, onDismiss, ...rest } = props
  return (
    <div className={cx('ui-toast', error && 'ui-toast-error')} tabIndex={-1} {...rest}>
      <Icon name={error ? 'error' : 'success'} />
      <p>{text}</p>
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
    if (toast.error) return
    timer.current = setTimeout(onDone, hasAction ? ACTION_MS : PLAIN_MS)
  }, [onDone, hasAction, toast.error])
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
          busy={busy}
          onClick={async () => {
            if (busy) return
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
