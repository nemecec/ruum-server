import type { DragEventHandler, FormEventHandler, ReactNode } from 'react'
import { NavLink } from 'react-router'
import { Button } from '../button/Button'
import { cx } from '../cx'
import { VisuallyHidden } from '../text/Text'
import './shell.css'

// The app shell (design-system 4.1): the top bar, the rail (a menu on the phone), the main region,
// and the layout parts of pages: the split view with the details panel, the selection bar, the drop
// zone and the floating upload panel.

/** "Skip to content": the first focusable element of every page. */
export function SkipLink({ target = 'main' }: { target?: string }) {
  return (
    <a className="ui-skip" href={`#${target}`}>
      Skip to content
    </a>
  )
}

/** The top bar: the wordmark, the space switcher, the search and the menu toggle on the phone. */
export function TopBar({ children }: { children: ReactNode }) {
  return (
    <header className="ui-topbar">
      <div className="ui-topbar-row">{children}</div>
    </header>
  )
}

/** The wordmark "Ruum": a link to Files, or plain text where there is no Files (a share link). */
export function Brand({ to, children }: { to?: string; children: ReactNode }) {
  if (!to) return <span className="ui-brand">{children}</span>
  return (
    <NavLink to={to} className="ui-brand">
      {children}
    </NavLink>
  )
}

/** A place in the top bar that takes the free width (the space switcher). */
export function TopBarItem({ children, grow }: { children: ReactNode; grow?: boolean }) {
  return <div className={cx('ui-topbar-item', grow && 'ui-topbar-item-grow')}>{children}</div>
}

/** The search form of the top bar (`role="search"`). */
export function SearchForm(props: {
  id: string
  label: string
  name: string
  defaultValue: string
  onSubmit: FormEventHandler<HTMLFormElement>
}) {
  return (
    <form role="search" className="ui-search" onSubmit={props.onSubmit}>
      <label htmlFor={props.id}>
        <VisuallyHidden>{props.label}</VisuallyHidden>
      </label>
      <input
        key={props.defaultValue}
        id={props.id}
        name={props.name}
        type="search"
        className="ui-search-input"
        placeholder={props.label}
        defaultValue={props.defaultValue}
      />
    </form>
  )
}

/** The "Menu" button that shows the navigation on the phone. Hidden on wider screens. */
export function MenuToggle({ open, controls, onToggle }: { open: boolean; controls: string; onToggle: () => void }) {
  return (
    <span className="ui-menu-toggle">
      <Button icon="menu" aria-expanded={open} aria-controls={controls} onClick={onToggle}>
        Menu
      </Button>
    </span>
  )
}

/** Below the top bar: the rail and the main region side by side (stacked on the phone). */
export function AppBody({ children }: { children: ReactNode }) {
  return <div className="ui-app-body">{children}</div>
}

/** The main navigation: a rail on the left; on the phone it shows only when the menu is open. */
export function NavRail({ id, label, open, children }: { id: string; label: string; open: boolean; children: ReactNode }) {
  return (
    <nav id={id} aria-label={label} className={cx('ui-rail', open && 'ui-rail-open')}>
      {children}
    </nav>
  )
}

/** A group of navigation items; `end` sits at the bottom of the rail (the account). */
export function NavList({ end, children }: { end?: boolean; children: ReactNode }) {
  return <ul className={cx('ui-nav-list', end && 'ui-nav-list-end')}>{children}</ul>
}

/** A navigation link; the current page has `aria-current="page"` and the selected look. */
export function NavItem({ to, onClick, children }: { to: string; onClick?: () => void; children: ReactNode }) {
  return (
    <li>
      <NavLink to={to} className="ui-nav-item" onClick={onClick}>
        {children}
      </NavLink>
    </li>
  )
}

/** A button in the navigation (Sign out). */
export function NavButton({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <li>
      <button type="button" className="ui-nav-item" onClick={onClick}>
        {children}
      </button>
    </li>
  )
}

/** The main region; the new page moves focus here (`tabIndex=-1`). */
export function AppMain({ id = 'main', children }: { id?: string; children: ReactNode }) {
  return (
    <main id={id} className="ui-main" tabIndex={-1}>
      {children}
    </main>
  )
}

/**
 * A list and the details of one item beside it (design-system 4.7). On the phone the details
 * panel is a sheet over the list, so it is always in view.
 */
export function SplitView({ children, aside, asideLabel }: { children: ReactNode; aside?: ReactNode; asideLabel: string }) {
  return (
    <div className={cx('ui-split', aside ? 'ui-split-with-aside' : undefined)}>
      <div className="ui-split-main">{children}</div>
      {aside && (
        <aside className="ui-split-aside" aria-label={asideLabel}>
          {aside}
        </aside>
      )}
    </div>
  )
}

/** The bar over a list while items are selected: the count and the bulk actions. */
export function SelectionBar({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="ui-selection-bar" role="region" aria-label={label}>
      {children}
    </div>
  )
}

/**
 * The drop zone (design-system 4.6): while files are dragged over a folder the person can change,
 * a dashed edge and "Drop to upload to *folder*". A visual aid only: "Upload" is the accessible way.
 */
export function DropZone(props: {
  active: boolean
  hint: string
  children: ReactNode
  onDragOver?: DragEventHandler
  onDragLeave?: DragEventHandler
  onDrop?: DragEventHandler
}) {
  const { active, hint, children, ...handlers } = props
  return (
    <div className={cx('ui-drop-zone', active && 'ui-drop-zone-active')} {...handlers}>
      {active && (
        <p className="ui-drop-hint" aria-hidden="true">
          {hint}
        </p>
      )}
      {children}
    </div>
  )
}

/**
 * A panel fixed at the bottom right (design-system 4.5, the upload panel): a header with a live
 * title and its buttons, and a body that folds.
 */
export function FloatingPanel(props: { label: string; title: ReactNode; actions: ReactNode; open: boolean; children: ReactNode }) {
  return (
    <section className="ui-floating-panel" aria-label={props.label}>
      <div className="ui-floating-head">
        <h2 aria-live="polite">{props.title}</h2>
        {props.actions}
      </div>
      <div className="ui-floating-body" hidden={!props.open}>
        {props.children}
      </div>
    </section>
  )
}
