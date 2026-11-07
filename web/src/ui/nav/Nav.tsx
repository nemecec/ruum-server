import type { ReactNode } from 'react'
import { NavLink } from 'react-router'
import './nav.css'
import { t } from '../../i18n'

/**
 * Breadcrumbs (design-system 4.3): a `nav` with an ordered list; the separators are CSS, not text.
 * The last crumb is the current place, with `aria-current`.
 */
export function Breadcrumbs({ label, hidden, children }: { label?: string; hidden?: boolean; children: ReactNode }) {
  return (
    <nav aria-label={label ?? t('ui.breadcrumbs')} className="ui-breadcrumbs" hidden={hidden}>
      <ol>{children}</ol>
    </nav>
  )
}

export function Crumb({ children }: { children: ReactNode }) {
  return <li>{children}</li>
}

/** The current crumb: plain text with `aria-current` (`page`, or `location` inside a picker). */
export function CurrentCrumb({ children, current = 'page' }: { children: ReactNode; current?: 'page' | 'location' }) {
  return (
    <li>
      <span aria-current={current}>{children}</span>
    </li>
  )
}

/** Sub-pages as tabs of links (Space settings). The active one has `aria-current="page"`. */
export function Tabs({ label, items }: { label: string; items: { to: string; label: string; end?: boolean }[] }) {
  return (
    <nav aria-label={label} className="ui-tabs">
      <ul>
        {items.map((i) => (
          <li key={i.to}>
            <NavLink to={i.to} end={i.end}>
              {i.label}
            </NavLink>
          </li>
        ))}
      </ul>
    </nav>
  )
}
