import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { errorText } from '../api/client'

/**
 * A modal dialog on the native `<dialog>` element: it traps the focus, closes with Escape and
 * gives the focus back to the button that opened it.
 */
export function Dialog(props: {
  title: string
  open: boolean
  onClose: () => void
  children: ReactNode
  wide?: boolean
}) {
  const ref = useRef<HTMLDialogElement>(null)
  const titleId = useId()
  const { open, onClose } = props

  useEffect(() => {
    const d = ref.current
    if (!d) return
    if (open && !d.open) {
      const opener = document.activeElement as HTMLElement | null
      d.showModal()
      return () => {
        if (d.open) d.close()
        opener?.focus?.()
      }
    }
  }, [open])

  if (!open) return null
  return (
    <dialog
      ref={ref}
      className={props.wide ? 'dialog wide' : 'dialog'}
      aria-labelledby={titleId}
      onCancel={(e) => {
        e.preventDefault()
        onClose()
      }}
    >
      <header className="dialog-head">
        <h2 id={titleId}>{props.title}</h2>
        <button type="button" className="icon" aria-label="Close" onClick={onClose}>
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
}) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>()
  return (
    <>
      <button
        type="button"
        className={props.className ?? (props.danger ? 'danger' : undefined)}
        aria-label={props.ariaLabel}
        onClick={() => {
          setError(undefined)
          setOpen(true)
        }}
      >
        {props.label}
      </button>
      <Dialog title={props.title} open={open} onClose={() => setOpen(false)}>
        <div className="stack">
          <div>{props.message}</div>
          <ErrorNote error={error} />
          <div className="actions">
            <button type="button" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button
              type="button"
              className={props.danger ? 'danger' : 'primary'}
              disabled={busy}
              onClick={async () => {
                setBusy(true)
                try {
                  await props.onConfirm()
                  setOpen(false)
                } catch (e) {
                  setError(e)
                } finally {
                  setBusy(false)
                }
              }}
            >
              {props.confirmLabel}
            </button>
          </div>
        </div>
      </Dialog>
    </>
  )
}

export function ErrorNote({ error }: { error: unknown }) {
  if (!error) return null
  return (
    <p className="error" role="alert">
      {errorText(error)}
    </p>
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
export function Loaded<T>(props: { state: { data: T | undefined; error: unknown; loading: boolean }; children: (data: T) => ReactNode }) {
  const { data, error, loading } = props.state
  if (error) return <ErrorNote error={error} />
  if (data === undefined) return loading ? <Loading /> : null
  return <>{props.children(data)}</>
}

/** A short status message for screen readers and sighted users (for example "Saved."). */
export function Notice({ text }: { text: string | null }) {
  return (
    <p className="notice" role="status" aria-live="polite">
      {text ?? ''}
    </p>
  )
}

/** A secret shown once, with a copy button. */
export function SecretBox({ label, secret }: { label: string; secret: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="secret">
      <label>
        {label}
        <input readOnly value={secret} onFocus={(e) => e.currentTarget.select()} />
      </label>
      <button
        type="button"
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(secret)
            setCopied(true)
          } catch {
            setCopied(false)
          }
        }}
      >
        {copied ? 'Copied' : 'Copy'}
      </button>
      <p className="muted">Copy it now. Ruum shows it only once.</p>
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
