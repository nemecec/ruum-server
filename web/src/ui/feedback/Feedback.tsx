import type { ReactNode } from 'react'
import { errorDetails, errorText, type ErrorContext } from '../../api/errors'
import { cx } from '../cx'
import { Icon, type IconName } from '../icon/Icon'
import './feedback.css'

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

/** An error note in plain words, with the code and the API's message under "Details". */
export function ErrorNote({ error, context }: { error: unknown; context?: ErrorContext }) {
  if (!error) return null
  const details = errorDetails(error)
  return (
    <div className="ui-note ui-note-danger ui-error-note" role="alert">
      <Icon name="error" />
      <div className="ui-note-body">
        <p className="ui-error-text">{errorText(error, context)}</p>
        {details && (
          <details className="ui-error-details">
            <summary>Details</summary>
            <pre>{details}</pre>
          </details>
        )}
      </div>
    </div>
  )
}

/** "Loading..." while a region loads. */
export function Loading({ label = 'Loading' }: { label?: string }) {
  return (
    <p className="ui-loading" role="status">
      <Spinner />
      {label}...
    </p>
  )
}

/** Shows loading and error states of `useApi` and renders `children` with the data. */
export function Loaded<T>(props: {
  state: { data: T | undefined; error: unknown; loading: boolean }
  children: (data: T) => ReactNode
  context?: ErrorContext
}) {
  const { data, error, loading } = props.state
  if (error) return <ErrorNote error={error} context={props.context} />
  if (data === undefined) return loading ? <Loading /> : null
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

/** Grey bars in the shape of the content that loads. Decorative. */
export function Skeleton({ lines = 3 }: { lines?: number }) {
  return (
    <div className="ui-skeleton" aria-hidden="true">
      {Array.from({ length: lines }, (_, i) => (
        <span key={i} className="ui-skeleton-line" />
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
