import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { errorDetails, errorText, type ErrorContext } from '../api/errors'
import { selectBaseName } from './dom'

/**
 * Where the focus goes when a dialog opens: an element marked `data-autofocus`, else the first
 * field of the body, else the first button of the body (for a confirm dialog that is "Cancel",
 * the least destructive action). Never the close button of the header.
 */
function firstFocus(d: HTMLDialogElement): HTMLElement | null {
  return (
    d.querySelector<HTMLElement>('[data-autofocus]') ??
    d.querySelector<HTMLElement>(
      '.dialog-body input:not([type=hidden]):not([disabled]):not([readonly]), .dialog-body select:not([disabled]), .dialog-body textarea:not([disabled])',
    ) ??
    d.querySelector<HTMLElement>('.dialog-body button:not([disabled])')
  )
}

/**
 * A modal dialog on the native `<dialog>` element: it traps the focus, closes with Escape, puts
 * the focus on the first field, and gives the focus back to the button that opened it.
 */
export function Dialog(props: {
  title: string
  open: boolean
  onClose: () => void
  children: ReactNode
  wide?: boolean
  full?: boolean
}) {
  const ref = useRef<HTMLDialogElement>(null)
  const titleId = useId()
  const { open } = props

  useEffect(() => {
    const d = ref.current
    if (!d) return
    if (open && !d.open) {
      const opener = document.activeElement as HTMLElement | null
      d.showModal()
      // showModal() focuses the first focusable element, which is the close button of the header.
      // Move the focus to where the person works.
      const target = firstFocus(d)
      if (target) {
        target.focus()
        if (target instanceof HTMLInputElement && target.dataset.selectBase !== undefined) selectBaseName(target)
      }
      return () => {
        if (d.open) d.close()
        opener?.focus?.()
      }
    }
  }, [open])

  if (!open) return null
  const cls = ['dialog', props.wide ? 'wide' : '', props.full ? 'full' : ''].filter(Boolean).join(' ')
  return (
    <dialog
      ref={ref}
      className={cls}
      aria-labelledby={titleId}
      onCancel={(e) => {
        e.preventDefault()
        props.onClose()
      }}
    >
      <header className="dialog-head">
        <h2 id={titleId}>{props.title}</h2>
        <button type="button" className="icon" aria-label="Close" onClick={props.onClose}>
          ×
        </button>
      </header>
      <div className="dialog-body">{props.children}</div>
    </dialog>
  )
}

/** A button that asks for confirmation in a dialog before it runs `onConfirm`. */
export function ConfirmButton(props: {
  label: string
  title: string
  message: ReactNode
  confirmLabel: string
  onConfirm: () => Promise<unknown> | void
  danger?: boolean
  className?: string
  ariaLabel?: string
  disabled?: boolean
}) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button
        type="button"
        className={props.className ?? (props.danger ? 'danger' : undefined)}
        aria-label={props.ariaLabel}
        disabled={props.disabled}
        onClick={() => setOpen(true)}
      >
        {props.label}
      </button>
      {open && (
        <ConfirmDialog
          title={props.title}
          message={props.message}
          confirmLabel={props.confirmLabel}
          danger={props.danger}
          onConfirm={props.onConfirm}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  )
}

/** A confirm dialog: Cancel (focused first) and the action. */
export function ConfirmDialog(props: {
  title: string
  message: ReactNode
  confirmLabel: string
  danger?: boolean
  onConfirm: () => Promise<unknown> | void
  onClose: () => void
}) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  return (
    <Dialog title={props.title} open onClose={props.onClose}>
      <div className="stack">
        <div>{props.message}</div>
        <ErrorNote error={error} />
        <div className="actions">
          <button type="button" onClick={props.onClose}>
            Cancel
          </button>
          <button
            type="button"
            className={props.danger ? 'danger' : 'primary'}
            disabled={busy}
            onClick={async () => {
              setBusy(true)
              setError(undefined)
              try {
                await props.onConfirm()
                props.onClose()
              } catch (e) {
                setError(e)
                setBusy(false)
              }
            }}
          >
            {props.confirmLabel}
          </button>
        </div>
      </div>
    </Dialog>
  )
}

/** An error note in plain words, with the code and the API's message under "Details". */
export function ErrorNote({ error, context }: { error: unknown; context?: ErrorContext }) {
  if (!error) return null
  const details = errorDetails(error)
  return (
    <div className="error-note" role="alert">
      <p className="error">{errorText(error, context)}</p>
      {details && (
        <details className="small muted">
          <summary>Details</summary>
          <pre>{details}</pre>
        </details>
      )}
    </div>
  )
}

export function Loading({ label = 'Loading' }: { label?: string }) {
  return (
    <p className="muted" role="status">
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
    <p className="notice" role="status" aria-live="polite">
      {text ?? ''}
    </p>
  )
}

/** An empty place: what it is for, and the next step. */
export function EmptyState(props: { title: string; text?: ReactNode; children?: ReactNode }) {
  return (
    <div className="empty-state">
      <p className="empty-title">{props.title}</p>
      {props.text && <p className="muted">{props.text}</p>}
      {props.children && <div className="actions center">{props.children}</div>}
    </div>
  )
}

/** A value with a copy button (a link, an address or a command). */
export function CopyField({ label, value, mono = true }: { label: string; value: string; mono?: boolean }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="secret">
      <label>
        {label}
        <input readOnly value={value} className={mono ? 'mono' : undefined} onFocus={(e) => e.currentTarget.select()} />
      </label>
      <button
        type="button"
        aria-label={`Copy ${label.toLowerCase()}`}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(value)
            setCopied(true)
          } catch {
            setCopied(false)
          }
        }}
      >
        {copied ? 'Copied' : 'Copy'}
      </button>
    </div>
  )
}

/** A secret shown once, with a copy button. */
export function SecretBox({ label, secret, note }: { label: string; secret: string; note?: string }) {
  return (
    <div className="stack">
      <CopyField label={label} value={secret} />
      <p className="muted small">{note ?? 'Copy it now. Ruum shows it only once.'}</p>
    </div>
  )
}

/** "Load more" under a paged list, while the server has more items (`next_cursor`). */
export function LoadMore({
  state,
}: {
  state: { data: { nextCursor: string | null } | undefined; loadMore: () => void; loadingMore: boolean; moreError: unknown }
}) {
  if (!state.data?.nextCursor) return null
  return (
    <div className="stack">
      <ErrorNote error={state.moreError} />
      <p className="actions">
        <span className="muted small">There are more items.</span>
        <button type="button" disabled={state.loadingMore} onClick={state.loadMore}>
          {state.loadingMore ? 'Loading...' : 'Load more'}
        </button>
      </p>
    </div>
  )
}

/** The glyph of a folder (an outline with a tab). Decorative. */
export function FolderIcon() {
  return (
    <svg className="glyph" viewBox="0 0 20 20" width="20" height="20" aria-hidden="true" focusable="false">
      <path d="M2.5 5.5a1 1 0 0 1 1-1h4l1.6 1.8h7.4a1 1 0 0 1 1 1v8.2a1 1 0 0 1-1 1h-13a1 1 0 0 1-1-1z" fill="currentColor" opacity="0.85" />
    </svg>
  )
}

const imageName = /\.(png|jpe?g|gif|webp|avif|heic|bmp|tiff?|svg)$/i

/**
 * The glyph of a file (a page with a folded corner and lines), or of a picture (a frame with a
 * hill and a sun) for images, so photos and notes differ at a glance. Decorative.
 */
export function FileIcon({ type, name }: { type?: string | null; name?: string } = {}) {
  if (type?.startsWith('image/') || (name && imageName.test(name))) {
    return (
      <svg className="glyph" viewBox="0 0 20 20" width="20" height="20" aria-hidden="true" focusable="false">
        <rect x="2.5" y="4" width="15" height="12" rx="1.2" fill="none" stroke="currentColor" strokeWidth="1.4" />
        <path d="M4.5 14l3.6-4.2 2.6 2.8 1.8-1.9 3 3.3" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinejoin="round" />
        <circle cx="13.3" cy="7.6" r="1.3" fill="currentColor" />
      </svg>
    )
  }
  return (
    <svg className="glyph" viewBox="0 0 20 20" width="20" height="20" aria-hidden="true" focusable="false">
      <path d="M5 2.5h6.5L15.5 6.5v11H5z" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinejoin="round" />
      <path d="M11.5 2.5v4h4M7.5 10h5M7.5 12.8h5" fill="none" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  )
}

/** The agent mark: a small spark glyph in the agent colour. Decorative; the text says "agent". */
export function AgentGlyph() {
  return (
    <svg className="agent-glyph" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true" focusable="false">
      <path d="M8 1.5l1.6 4.2 4.4.3-3.4 2.8 1.1 4.3L8 10.7l-3.7 2.4 1.1-4.3L2 6l4.4-.3z" fill="currentColor" />
    </svg>
  )
}
