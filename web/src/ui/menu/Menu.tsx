import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { cx } from '../cx'
import { Icon } from '../icon/Icon'
import './menu.css'

export type MenuItem = { label: string; hint?: string; onSelect: () => void; disabled?: boolean; checked?: boolean; danger?: boolean }

/**
 * A menu button (design-system 4.2, 4.4 row menus): Up and Down arrows, Home, End, Enter, Escape
 * and type-ahead. The menu closes on a choice, Escape or a click outside; the focus goes back to
 * the button.
 */
export function Menu(props: { label: ReactNode; ariaLabel?: string; items: MenuItem[]; align?: 'start' | 'end'; quiet?: boolean }) {
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const button = useRef<HTMLButtonElement>(null)
  const list = useRef<HTMLUListElement>(null)
  const root = useRef<HTMLDivElement>(null)
  const id = useId()

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

  function openAt(i: number) {
    setActive(i)
    setOpen(true)
  }

  function close() {
    setOpen(false)
    button.current?.focus()
  }

  function onKey(e: KeyboardEvent) {
    const n = props.items.length
    if (e.key === 'ArrowDown') setActive((a) => (a + 1) % n)
    else if (e.key === 'ArrowUp') setActive((a) => (a - 1 + n) % n)
    else if (e.key === 'Home') setActive(0)
    else if (e.key === 'End') setActive(n - 1)
    else if (e.key === 'Escape' || e.key === 'Tab') close()
    else if (e.key.length === 1) {
      const i = props.items.findIndex((it) => it.label.toLowerCase().startsWith(e.key.toLowerCase()))
      if (i >= 0) setActive(i)
      return
    } else return
    e.preventDefault()
  }

  const hasChecks = props.items.some((i) => i.checked !== undefined)
  return (
    <div className="ui-menu" ref={root}>
      <button
        ref={button}
        type="button"
        className={cx('ui-menu-button', props.quiet && 'ui-menu-button-quiet')}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-label={props.ariaLabel}
        onClick={() => (open ? setOpen(false) : openAt(0))}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') {
            e.preventDefault()
            openAt(0)
          } else if (e.key === 'ArrowUp') {
            e.preventDefault()
            openAt(props.items.length - 1)
          }
        }}
      >
        {props.label}
        <Icon name="chevronDown" small />
      </button>
      {open && (
        <ul ref={list} id={id} role="menu" className={cx('ui-menu-list', props.align === 'end' && 'ui-menu-end')} onKeyDown={onKey}>
          {props.items.map((item, i) => (
            <li key={item.label} role="none">
              <button
                type="button"
                role={hasChecks ? 'menuitemradio' : 'menuitem'}
                aria-checked={hasChecks ? Boolean(item.checked) : undefined}
                tabIndex={i === active ? 0 : -1}
                disabled={item.disabled}
                className={cx('ui-menu-item', item.danger && 'ui-menu-item-danger')}
                onClick={() => {
                  close()
                  item.onSelect()
                }}
              >
                <span className="ui-menu-check">{item.checked && <Icon name="check" small />}</span>
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
