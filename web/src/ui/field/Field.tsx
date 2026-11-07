import {
  forwardRef,
  useId,
  useState,
  type FieldsetHTMLAttributes,
  type InputHTMLAttributes,
  type ReactNode,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from 'react'
import { Button } from '../button/Button'
import { cx } from '../cx'
import { Icon } from '../icon/Icon'
import '../text/text.css'
import './field.css'
import { t } from '../../i18n'

/**
 * A labelled field (design-system 4.15): the label above the control, helper text and the error
 * under it. The label wraps the control, so the label text is the control's name.
 * `hint` and `error` are linked to the control with `aria-describedby` by the caller (`hintId`).
 */
export function Field(props: {
  label: ReactNode
  children: ReactNode
  hint?: ReactNode
  error?: ReactNode
  hintId?: string
  errorId?: string
  grow?: boolean
  /** The label is for screen readers only (a field whose place says what it is: the space switcher). */
  labelHidden?: boolean
}) {
  return (
    <div className={cx('ui-field', props.grow && 'ui-field-grow')}>
      <label className="ui-field-label">
        <span className={props.labelHidden ? 'ui-visually-hidden' : 'ui-field-text'}>{props.label}</span>
        {props.children}
      </label>
      {props.hint && (
        <p className="ui-field-hint" id={props.hintId}>
          {props.hint}
        </p>
      )}
      {props.error && (
        <p className="ui-field-error" id={props.errorId}>
          <Icon name="error" small />
          {props.error}
        </p>
      )}
    </div>
  )
}

type Width = 'sm' | 'md' | 'full'

/** A text input: text, email, search, password, date, number, url. */
export const TextInput = forwardRef<
  HTMLInputElement,
  Omit<InputHTMLAttributes<HTMLInputElement>, 'className' | 'style' | 'width'> & { width?: Width; mono?: boolean }
>(function TextInput({ width = 'full', mono, ...rest }, ref) {
  return <input ref={ref} className={cx('ui-input', `ui-width-${width}`, mono && 'ui-input-mono')} {...rest} />
})

/**
 * The hidden file input behind an "Upload" button: the button calls `click()` on it. `label` is
 * its accessible name.
 */
export const FileInput = forwardRef<
  HTMLInputElement,
  Omit<InputHTMLAttributes<HTMLInputElement>, 'className' | 'style' | 'type' | 'hidden'> & { label: string }
>(function FileInput({ label, ...rest }, ref) {
  return <input ref={ref} type="file" hidden aria-label={label} {...rest} />
})

/** A native select, for simple lists. */
export const Select = forwardRef<
  HTMLSelectElement,
  Omit<SelectHTMLAttributes<HTMLSelectElement>, 'className' | 'style'> & { width?: Width | 'auto' }
>(function Select({ width = 'full', ...rest }, ref) {
  return <select ref={ref} className={cx('ui-input', 'ui-select', `ui-width-${width}`)} {...rest} />
})

export const TextArea = forwardRef<
  HTMLTextAreaElement,
  Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, 'className' | 'style'> & { mono?: boolean }
>(function TextArea({ mono, ...rest }, ref) {
  return <textarea ref={ref} className={cx('ui-input', 'ui-textarea', mono && 'ui-input-mono')} {...rest} />
})

type CheckProps = Omit<InputHTMLAttributes<HTMLInputElement>, 'className' | 'style' | 'type'> & { label: ReactNode }

/** A checkbox with its label on the right. Without `label` text it needs `aria-label`. */
export const Checkbox = forwardRef<HTMLInputElement, CheckProps>(function Checkbox({ label, ...rest }, ref) {
  return (
    <label className="ui-check">
      <input ref={ref} type="checkbox" className="ui-check-input" {...rest} />
      {label}
    </label>
  )
})

/** A checkbox without a visible label (a row's "Select *name*"): `aria-label` is required. */
export const BareCheckbox = forwardRef<
  HTMLInputElement,
  Omit<InputHTMLAttributes<HTMLInputElement>, 'className' | 'style' | 'type'> & { 'aria-label': string }
>(function BareCheckbox(props, ref) {
  // The label around the box fills the check cell, so a tap beside the box counts (44 px on touch).
  return (
    <label className="ui-check-cell-label">
      <input ref={ref} type="checkbox" className="ui-check-input" {...props} />
    </label>
  )
})

/** A radio with its label on the right. Group radios in a Fieldset. */
export const Radio = forwardRef<HTMLInputElement, CheckProps>(function Radio({ label, ...rest }, ref) {
  return (
    <label className="ui-check">
      <input ref={ref} type="radio" className="ui-check-input" {...rest} />
      {label}
    </label>
  )
})

/** A group of controls with a legend (radio groups, a set of related fields). */
export function Fieldset({
  legend,
  plain,
  children,
  ...rest
}: { legend: ReactNode; plain?: boolean; children: ReactNode } & Omit<FieldsetHTMLAttributes<HTMLFieldSetElement>, 'className' | 'style'>) {
  return (
    <fieldset className={cx('ui-fieldset', plain && 'ui-fieldset-plain')} {...rest}>
      <legend className="ui-legend">{legend}</legend>
      {children}
    </fieldset>
  )
}

/** An on/off switch (`role="switch"`) with its label and the state in words. */
export function Switch(props: { label: ReactNode; checked: boolean; onChange: (on: boolean) => void; disabled?: boolean }) {
  const id = useId()
  return (
    <span className="ui-switch-row">
      <button
        type="button"
        role="switch"
        id={id}
        aria-checked={props.checked}
        disabled={props.disabled}
        className="ui-switch"
        onClick={() => props.onChange(!props.checked)}
      >
        <span className="ui-switch-thumb" />
      </button>
      <label htmlFor={id} className="ui-switch-label">
        {props.label}
      </label>
    </span>
  )
}

/**
 * A value with a copy button (a link, an address or a command). `wrap`: a long value (a command)
 * wraps onto more lines instead of being cut, so the person can read what they copy.
 */
export function CopyField({ label, value, mono = true, wrap }: { label: string; value: string; mono?: boolean; wrap?: boolean }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="ui-copy-field">
      <Field label={label} grow>
        {wrap ? (
          // The name is the label alone: a label's text would include the text of the textarea.
          <TextArea
            readOnly
            aria-label={label}
            value={value}
            mono={mono}
            rows={Math.min(4, Math.ceil(value.length / 48))}
            onFocus={(e) => e.currentTarget.select()}
          />
        ) : (
          <TextInput readOnly value={value} mono={mono} onFocus={(e) => e.currentTarget.select()} />
        )}
      </Field>
      <Button
        aria-label={t('ui.copy.label', { label })}
        icon={copied ? 'check' : 'copy'}
        onClick={async () => {
          try {
            await navigator.clipboard.writeText(value)
            setCopied(true)
          } catch {
            setCopied(false)
          }
        }}
      >
        {copied ? t('ui.copy.done') : t('ui.copy')}
      </Button>
    </div>
  )
}

/** A secret shown once, with a copy button. */
export function SecretBox({ label, secret, note }: { label: string; secret: string; note?: string }) {
  return (
    <div className="ui-secret">
      <CopyField label={label} value={secret} />
      <p className="ui-field-hint">{note ?? t('ui.secret.note')}</p>
    </div>
  )
}
