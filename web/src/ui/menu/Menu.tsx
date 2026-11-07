import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { cx } from '../cx'
import { Icon, type IconName } from '../icon/Icon'
import '../text/text.css'
import './menu.css'

export type MenuItem = {
  label: string
  /** A second line: what the choice means (a role menu) or more about it (a space's role). */
  hint?: string
  onSelect: () => void
  disabled?: boolean
  checked?: boolean
  danger?: boolean
}

/**
 * A menu button (design-system 4.2, 4.4 row menus): Up and Down arrows, Home, End, Enter, Escape
 * and type-ahead. The menu closes on a choice, Escape or a click outside; the focus goes back to
 * the button. Tab closes it and moves on. A disabled item can take the focus (`aria-disabled`), so
 * a screen reader still hears it, but it cannot be chosen. A menu of choices (`checked`) opens on
 * the checked item.
 *
 * The button's accessible name starts with its visible text (WCAG 2.5.3): `context` adds words
 * after it ("Can edit, access of Liis Tamm"). `ariaLabel` is only for a button without text
 * (`icon` and no `label`), such as the row menu "More actions for *name*".
 */
export function Menu(props: {
  label?: ReactNode
  /** Words that follow the visible label in the accessible name. */
  context?: string
  ariaLabel?: string
  /** An icon-only button (the row menu): `ariaLabel` names it. */
  icon?: IconName
  items: MenuItem[]
  align?: 'start' | 'end'
  quiet?: boolean
  /** The button fills its place and cuts a long label with an ellipsis (the space switcher). */
  wide?: boolean
  /** The tooltip of the button (the full text of a cut label). */
  title?: string
  size?: 'sm' | 'md'
  disabled?: boolean
}) {
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const button = useRef<HTMLButtonElement>(null)
  const list = useRef<HTMLUListElement>(null)
  const root = useRef<HTMLDivElement>(null)
  const id = useId()
  const buttonId = `${id}-button`
  const contextId = `${id}-context`
  const { items } = props

  useEffect(() => {
    if (!open) return
    list.current?.querySelectorAll<HTMLElement>('[role=menuitem], [role=menuitemradio]')[active]?.focus()
  }, [open, active])

  useEffect(() => {
    if (!open) return
    const close = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('mousedown', close)
    return () => document.removeEventListener('mousedown', close)
  }, [open])

  const enabled = (i: number) => !items[i]?.disabled

  /** The next item that can be chosen from `from` in `step` direction (wraps); `from` when none. */
  function step(from: number, dir: 1 | -1): number {
    const n = items.length
    for (let k = 1; k <= n; k++) {
      const i = (from + dir * k + n * k) % n
      if (enabled(i)) return i
    }
    return from
  }

  function first(): number {
    const checked = items.findIndex((it) => it.checked && !it.disabled)
    if (checked >= 0) return checked
    const i = items.findIndex((it) => !it.disabled)
    return i >= 0 ? i : 0
  }

  function openAt(i: number) {
    setActive(i)
    setOpen(true)
  }

  function close() {
    setOpen(false)
    button.current?.focus()
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'ArrowDown') setActive((a) => step(a, 1))
    else if (e.key === 'ArrowUp') setActive((a) => step(a, -1))
    else if (e.key === 'Home') setActive(step(-1, 1))
    else if (e.key === 'End') setActive(step(items.length, -1))
    else if (e.key === 'Escape') {
      // Only the menu closes, not the panel or dialog around it.
      e.stopPropagation()
      close()
    }
    else if (e.key === 'Tab') {
      // The menu pattern: Tab leaves the menu and moves on from the button.
      close()
      return
    } else if (e.key.length === 1) {
      const key = e.key.toLocaleLowerCase() // i18n-ignore: matching typed keys, not text people see
      const i = items.findIndex((it) => !it.disabled && it.label.toLocaleLowerCase().startsWith(key)) // i18n-ignore: matching only
      if (i >= 0) setActive(i)
      return
    } else return
    e.preventDefault()
  }

  const hasChecks = items.some((i) => i.checked !== undefined)
  const iconOnly = props.icon && props.label === undefined
  return (
    <div className={cx('ui-menu', props.wide && 'ui-menu-wide')} ref={root}>
      <button
        ref={button}
        id={buttonId}
        type="button"
        className={cx(
          'ui-menu-button',
          props.quiet && 'ui-menu-button-quiet',
          iconOnly && 'ui-menu-button-icon',
          props.size === 'sm' && 'ui-menu-button-sm',
        )}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-label={iconOnly ? props.ariaLabel : undefined}
        aria-labelledby={!iconOnly && props.context ? `${buttonId} ${contextId}` : undefined}
        title={props.title ?? (iconOnly ? props.ariaLabel : undefined)}
        disabled={props.disabled}
        onClick={() => (open ? setOpen(false) : openAt(first()))}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') {
            e.preventDefault()
            openAt(first())
          } else if (e.key === 'ArrowUp') {
            e.preventDefault()
            openAt(step(items.length, -1))
          }
        }}
      >
        {props.icon && <Icon name={props.icon} />}
        {props.label !== undefined && <span className="ui-menu-label">{props.label}</span>}
        {!iconOnly && <Icon name="chevronDown" small />}
      </button>
      {props.context && !iconOnly && (
        <span id={contextId} className="ui-visually-hidden">
          {props.context}
        </span>
      )}
      {open && (
        <ul
          ref={list}
          id={id}
          role="menu"
          aria-labelledby={buttonId}
          className={cx('ui-menu-list', props.align === 'end' && 'ui-menu-end')}
          onKeyDown={onKey}
        >
          {items.map((item, i) => (
            <li key={item.label} role="none">
              <button
                type="button"
                role={hasChecks && item.checked !== undefined ? 'menuitemradio' : 'menuitem'}
                aria-checked={hasChecks && item.checked !== undefined ? Boolean(item.checked) : undefined}
                aria-disabled={item.disabled || undefined}
                tabIndex={i === active ? 0 : -1}
                className={cx('ui-menu-item', item.danger && 'ui-menu-item-danger')}
                onClick={() => {
                  if (item.disabled) return
                  close()
                  item.onSelect()
                }}
              >
                {hasChecks && <span className="ui-menu-check">{item.checked && <Icon name="check" small />}</span>}
                <span className="ui-menu-text">
                  {item.label}
                  {item.hint && <span className="ui-menu-hint">{item.hint}</span>}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
