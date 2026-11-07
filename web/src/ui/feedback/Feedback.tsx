import { useEffect, useState, type ReactNode } from 'react'
import { errorDetails, errorText, type ErrorContext } from '../../api/errors'
import { Button } from '../button/Button'
import { cx } from '../cx'
import { Icon, type IconName } from '../icon/Icon'
import '../text/text.css'
import './feedback.css'
import { t } from '../../i18n'

/** A small turning circle while something works. Decorative; the text beside it says what. */
export function Spinner() {
  return <span className="ui-spinner" aria-hidden="true" />
}

export type NoteTone = 'info' | 'success' | 'warning' | 'danger'

const noteIcons: Record<NoteTone, IconName> = { info: 'info', success: 'success', warning: 'warning', danger: 'error' }

/**
 * An inline note (design-system 4.14): a 3 px edge, a soft background, an icon and the text.
 * `role="alert"` for a note that appears after an action; none for a note present on load.
 */
export function Note(props: { tone?: NoteTone; role?: 'alert' | 'status' | 'note'; id?: string; children: ReactNode; action?: ReactNode }) {
  const tone = props.tone ?? 'info'
  return (
    <div className={cx('ui-note', `ui-note-${tone}`)} role={props.role} id={props.id}>
      <Icon name={noteIcons[tone]} />
      <div className="ui-note-body">{props.children}</div>
      {props.action && <div className="ui-note-action">{props.action}</div>}
    </div>
  )
}

/**
 * An error note in plain words: what happened and what to do (journeys, common states). With
 * `onRetry` it offers "Try again" when a new try can help (network, server errors). The code and
 * the API's message are under "Details".
 */
export function ErrorNote({ error, context, onRetry }: { error: unknown; context?: ErrorContext; onRetry?: () => void }) {
  if (!error) return null
  const details = errorDetails(error)
  const retry = onRetry && retryable(error)
  return (
    <div className="ui-note ui-note-danger ui-error-note" role="alert">
      <Icon name="error" />
      <div className="ui-note-body">
        <p className="ui-error-text">{errorText(error, context)}</p>
        {retry && (
          <div className="ui-error-retry">
            <Button size="sm" icon="refresh" onClick={onRetry}>
              {t('ui.error.retry')}
            </Button>
          </div>
        )}
        {details && (
          <details className="ui-error-details">
            <summary>{t('ui.error.details')}</summary>
            <pre>{details}</pre>
          </details>
        )}
      </div>
    </div>
  )
}

/** "Loading..." while a region loads. */
export function Loading({ label }: { label?: string }) {
  return (
    <p className="ui-loading" role="status">
      <Spinner />
      {t('ui.loading', { label: label ?? t('ui.loading.default') })}
    </p>
  )
}

/** A new try can help: no answer (offline), a timeout, a limit or a server error. */
function retryable(error: unknown): boolean {
  const status = (error as { status?: unknown } | null)?.status
  if (typeof status !== 'number') return true
  return status === 0 || status === 408 || status === 429 || status >= 500
}

/** How long a load may take before the page shows that it loads (journeys, common states). */
export const LOADING_DELAY_MS = 300

/** True once `active` has been true for `ms`; false again at once when it ends. */
function useDelayed(active: boolean, ms: number): boolean {
  const [shown, setShown] = useState(false)
  useEffect(() => {
    if (!active) {
      setShown(false)
      return
    }
    const id = setTimeout(() => setShown(true), ms)
    return () => clearTimeout(id)
  }, [active, ms])
  return active && shown
}

/**
 * Shows loading and error states of `useApi` and renders `children` with the data. Nothing shows
 * for the first 300 ms; then a skeleton in the shape of the content (`skeleton` lines), or
 * "Loading..." for a small region. An error offers "Try again" with the state's `reload`.
 */
export function Loaded<T>(props: {
  state: { data: T | undefined; error: unknown; loading: boolean; reload?: () => void }
  children: (data: T) => ReactNode
  context?: ErrorContext
  /** The lines of the skeleton while it loads; without it a "Loading..." line. */
  skeleton?: number
  label?: string
}) {
  const { data, error, loading, reload } = props.state
  const waiting = data === undefined && !error && loading
  const show = useDelayed(waiting, LOADING_DELAY_MS)
  if (error) return <ErrorNote error={error} context={props.context} onRetry={reload} />
  if (data === undefined) {
    if (!show) return null
    return props.skeleton ? <Skeleton lines={props.skeleton} label={props.label} /> : <Loading label={props.label} />
  }
  return <>{props.children(data)}</>
}

/** A short status message inside a dialog or a form (for example "Link copied."). */
export function Notice({ text }: { text: string | null }) {
  return (
    <p className="ui-notice" role="status" aria-live="polite">
      {text ?? ''}
    </p>
  )
}

/** Grey bars in the shape of the content that loads. The bars are decorative; a hidden status says "Loading...". */
export function Skeleton({ lines = 3, label }: { lines?: number; label?: string }) {
  return (
    <div className="ui-skeleton">
      <span className="ui-visually-hidden" role="status">
        {t('ui.loading', { label: label ?? t('ui.loading.default') })}
      </span>
      {Array.from({ length: lines }, (_, i) => (
        <span key={i} className="ui-skeleton-line" aria-hidden="true" />
      ))}
    </div>
  )
}

/** An empty place (design-system 4.11): what it is for, and the next step. */
export function EmptyState(props: { title: string; text?: ReactNode; icon?: IconName; children?: ReactNode }) {
  return (
    <div className="ui-empty">
      {props.icon && (
        <span className="ui-empty-icon">
          <Icon name={props.icon} />
        </span>
      )}
      <p className="ui-empty-title">{props.title}</p>
      {props.text && <p className="ui-empty-text">{props.text}</p>}
      {props.children && <div className="ui-empty-actions">{props.children}</div>}
    </div>
  )
}

/** A bar across the page under the top bar (offline). It pushes the content down. */
export function Banner({ children, role = 'status' }: { children: ReactNode; role?: 'status' | 'alert' }) {
  return (
    <p className="ui-banner" role={role}>
      {children}
    </p>
  )
}

/** A labelled progress bar (an upload). */
export function ProgressBar({ value, max, label }: { value: number; max: number; label: string }) {
  return <progress className="ui-progress" max={max} value={value} aria-label={label} />
}

/** A meter of a used amount against a limit (storage, members). */
export function Meter({ value, max, label, high }: { value: number; max: number; label: string; high?: number }) {
  return <meter className="ui-meter" min={0} max={max} value={value} high={high} aria-label={label} />
}
