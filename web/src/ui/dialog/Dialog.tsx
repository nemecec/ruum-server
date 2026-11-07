import { useEffect, useId, useRef, useState, type ReactNode } from 'react'
import { selectBaseName } from '../../components/dom'
import { Button, IconButton, type ButtonVariant } from '../button/Button'
import { cx } from '../cx'
import { ErrorNote } from '../feedback/Feedback'
import { Actions, Stack } from '../layout/Layout'
import './dialog.css'

/**
 * Where the focus goes when a dialog opens: an element marked `data-autofocus`, else the first
 * field of the body, else the first button of the body (for a confirm dialog that is "Cancel",
 * the least destructive action). Never the close button of the header.
 */
function firstFocus(d: HTMLDialogElement): HTMLElement | null {
  return (
    d.querySelector<HTMLElement>('[data-autofocus]') ??
    d.querySelector<HTMLElement>(
      '.ui-dialog-body input:not([type=hidden]):not([disabled]):not([readonly]), .ui-dialog-body select:not([disabled]), .ui-dialog-body textarea:not([disabled])',
    ) ??
    d.querySelector<HTMLElement>('.ui-dialog-body button:not([disabled])')
  )
}

/**
 * A modal dialog on the native `<dialog>` element (design-system 4.12): it traps the focus,
 * closes with Escape, puts the focus on the first field, and gives the focus back to the button
 * that opened it. Sizes: `sm` 480 px (confirm, forms), `md` 720 px (share), `full` (compare; a
 * full page on the phone). On the phone every dialog is a sheet from the bottom.
 */
export function Dialog(props: {
  title: string
  open: boolean
  onClose: () => void
  children: ReactNode
  size?: 'sm' | 'md' | 'full'
  /** The id of the text that describes the dialog (confirm dialogs). */
  describedBy?: string
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
  return (
    <dialog
      ref={ref}
      className={cx('ui-dialog', `ui-dialog-${props.size ?? 'sm'}`)}
      aria-labelledby={titleId}
      aria-describedby={props.describedBy}
      onCancel={(e) => {
        e.preventDefault()
        props.onClose()
      }}
    >
      <header className="ui-dialog-head">
        <h2 id={titleId}>{props.title}</h2>
        <IconButton icon="close" label="Close" onClick={props.onClose} />
      </header>
      <div className="ui-dialog-body">{props.children}</div>
    </dialog>
  )
}

/** A confirm dialog: Cancel (focused first) and the action, which names the verb and the object. */
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
  const messageId = useId()
  return (
    <Dialog title={props.title} open onClose={props.onClose} describedBy={messageId}>
      <Stack gap={4}>
        <div id={messageId}>{props.message}</div>
        <ErrorNote error={error} />
        <Actions>
          <Button onClick={props.onClose}>Cancel</Button>
          <Button
            variant={props.danger ? 'danger' : 'primary'}
            disabled={busy}
            busy={busy}
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
          </Button>
        </Actions>
      </Stack>
    </Dialog>
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
  /** The look of the opening button; secondary by default (the dialog carries the danger). */
  variant?: ButtonVariant
  size?: 'sm' | 'md'
  ariaLabel?: string
  disabled?: boolean
}) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <Button
        variant={props.variant}
        size={props.size}
        aria-label={props.ariaLabel}
        disabled={props.disabled}
        onClick={() => setOpen(true)}
      >
        {props.label}
      </Button>
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
