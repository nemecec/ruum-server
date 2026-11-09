import {
  createElement,
  useEffect,
  useRef,
  useSyncExternalStore,
  type DragEventHandler,
  type FormEventHandler,
  type ReactNode,
} from 'react'
import { NavLink } from 'react-router'
import { Button } from '../button/Button'
import { cx } from '../cx'
import { VisuallyHidden } from '../text/Text'
import { wordmark, type BrandShape } from '../../design/generated/brand'
import './shell.css'
import { t } from '../../i18n'

// The app shell (design-system 4.1): the top bar, the rail (a menu on the phone), the main region,
// and the layout parts of pages: the split view with the details panel, the selection bar, the drop
// zone and the floating upload panel.

/** "Skip to content": the first focusable element of every page. */
export function SkipLink({ target = 'main' }: { target?: string }) {
  return (
    <a className="ui-skip" href={`#${target}`}>
      {t('ui.skip')}
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

/**
 * The wordmark "Ruum": a link to Files, or plain text where there is no Files (a share link). The
 * brand is not translated. `label` names the link by where it goes ("Ruum, files"), so it is never
 * read as the same word as a label next to it.
 */
export function Brand({ to, label, children }: { to?: string; label?: string; children: ReactNode }) {
  // The theme's wordmark (design 12.6) is drawn; the name stays as text for screen readers.
  const content = wordmark ? (
    <>
      <Wordmark />
      <VisuallyHidden>{children}</VisuallyHidden>
    </>
  ) : (
    children
  )
  if (!to) return <span className="ui-brand">{content}</span>
  return (
    <NavLink to={to} className="ui-brand" aria-label={label}>
      {content}
    </NavLink>
  )
}

/** One shape of the wordmark (plain SVG shapes; check-theme.mjs allows nothing else). */
function shape(node: BrandShape, key: number): ReactNode {
  return createElement(node.tag, { key, ...node.props }, ...node.children.map(shape))
}

/** The theme's wordmark, coloured by the colour tokens, so it follows light and dark. Decorative. */
function Wordmark() {
  if (!wordmark) return null
  return (
    <svg className="ui-wordmark" {...wordmark.props} aria-hidden="true" focusable="false">
      {wordmark.children.map(shape)}
    </svg>
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
    <form role="search" aria-label={props.label} className="ui-search" onSubmit={props.onSubmit}>
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
        {t('ui.menu')}
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

/** The width under which the layout is the phone layout (the CSS uses the same 640 px). */
export const NARROW_QUERY = '(max-width: 640px)'

function subscribeNarrow(onChange: () => void) {
  if (typeof window === 'undefined' || !window.matchMedia) return () => {}
  const q = window.matchMedia(NARROW_QUERY)
  q.addEventListener?.('change', onChange)
  return () => q.removeEventListener?.('change', onChange)
}

/** True on the phone layout. */
export function useNarrow(): boolean {
  return useSyncExternalStore(
    subscribeNarrow,
    () => (typeof window !== 'undefined' && window.matchMedia ? window.matchMedia(NARROW_QUERY).matches : false),
    () => false,
  )
}

/**
 * A list and the details of one item beside it (design-system 4.7). On the phone the details
 * panel is a modal sheet over the list: the list is `inert` while the sheet is open, so the focus
 * never goes under the sheet (WCAG 2.4.11), and Escape closes it (`onCloseAside`). When the panel
 * closes, the focus goes back to where it was when the panel opened (the row).
 */
export function SplitView({
  children,
  aside,
  asideLabel,
  onCloseAside,
}: {
  children: ReactNode
  aside?: ReactNode
  asideLabel: string
  onCloseAside?: () => void
}) {
  const narrow = useNarrow()
  const open = Boolean(aside)
  const opener = useRef<HTMLElement | null>(null)
  const asideRef = useRef<HTMLElement>(null)

  useEffect(() => {
    if (!open) return
    const before = document.activeElement
    opener.current = before instanceof HTMLElement && !asideRef.current?.contains(before) ? before : null
    return () => {
      const back = opener.current
      // Only when the focus was in the panel or got lost with it.
      const active = document.activeElement
      if (back && back.isConnected && (!active || active === document.body || !active.isConnected)) back.focus()
    }
  }, [open])

  return (
    <div className={cx('ui-split', open && 'ui-split-with-aside', open && narrow && 'ui-split-modal')}>
      <div className="ui-split-main" inert={open && narrow ? true : undefined}>
        {children}
      </div>
      {aside && (
        <aside
          ref={asideRef}
          className="ui-split-aside"
          aria-label={asideLabel}
          role={narrow ? 'dialog' : undefined}
          aria-modal={narrow ? true : undefined}
          onKeyDown={(e) => {
            if (e.key === 'Escape' && onCloseAside) {
              e.stopPropagation()
              onCloseAside()
            }
          }}
        >
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
