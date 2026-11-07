import { useLayoutEffect, useRef, useState, type DragEventHandler, type ReactNode, type ThHTMLAttributes, type TdHTMLAttributes } from 'react'
import { cx } from '../cx'
import { VisuallyHidden } from '../text/Text'
import './table.css'

/**
 * A data table with a caption (visually hidden unless `showCaption`). When the table is wider than
 * its place (a narrow screen, long words), its wrapper scrolls sideways and becomes a focusable,
 * named region, so keyboard users can scroll it too (WCAG 1.4.10, 2.1.1).
 */
export function Table({ caption, showCaption, children }: { caption: string; showCaption?: boolean; children: ReactNode }) {
  const wrap = useRef<HTMLDivElement>(null)
  const [scrolls, setScrolls] = useState(false)
  useLayoutEffect(() => {
    const el = wrap.current
    if (!el) return
    const check = () => setScrolls(el.scrollWidth > el.clientWidth + 1)
    check()
    if (typeof ResizeObserver === 'undefined') return
    const ro = new ResizeObserver(check)
    ro.observe(el)
    if (el.firstElementChild) ro.observe(el.firstElementChild)
    return () => ro.disconnect()
  }, [])
  return (
    <div
      ref={wrap}
      className="ui-table-wrap"
      tabIndex={scrolls ? 0 : undefined}
      role={scrolls ? 'region' : undefined}
      aria-label={scrolls ? caption : undefined}
    >
      <table className="ui-table">
        {showCaption ? (
          <caption className="ui-table-caption">{caption}</caption>
        ) : (
          <caption>
            <VisuallyHidden>{caption}</VisuallyHidden>
          </caption>
        )}
        {children}
      </table>
    </div>
  )
}

export function THead({ children }: { children: ReactNode }) {
  return <thead>{children}</thead>
}

export function TBody({ children }: { children: ReactNode }) {
  return <tbody>{children}</tbody>
}

/**
 * A row. `selected`: the selected row background. `dropTarget`: the dashed outline while files are
 * dragged over a folder row.
 */
export function Tr(props: {
  selected?: boolean
  dropTarget?: boolean
  children: ReactNode
  onDragOver?: DragEventHandler
  onDrop?: DragEventHandler
}) {
  const { selected, dropTarget, ...rest } = props
  return <tr className={cx(selected && 'ui-row-selected', dropTarget && 'ui-row-drop') || undefined} {...rest} />
}

type CellOptions = {
  /** Hidden on the phone (the column is extra there). */
  hideNarrow?: boolean
  /** Numbers: right aligned, tabular figures. */
  numeric?: boolean
  /** The checkbox column. */
  check?: boolean
  /** The row's buttons, at the end. */
  actions?: boolean
}

function cellClass({ hideNarrow, numeric, check, actions }: CellOptions) {
  return (
    cx(hideNarrow && 'ui-hide-narrow', numeric && 'ui-cell-numeric', check && 'ui-cell-check', actions && 'ui-cell-actions') || undefined
  )
}

export function Th({
  hideNarrow,
  numeric,
  check,
  actions,
  ...rest
}: CellOptions & Omit<ThHTMLAttributes<HTMLTableCellElement>, 'className' | 'style'>) {
  return <th className={cellClass({ hideNarrow, numeric, check, actions })} {...rest} />
}

export function Td({
  hideNarrow,
  numeric,
  check,
  actions,
  ...rest
}: CellOptions & Omit<TdHTMLAttributes<HTMLTableCellElement>, 'className' | 'style'>) {
  return <td className={cellClass({ hideNarrow, numeric, check, actions })} {...rest} />
}

/** A column header that sorts (`aria-sort` on the header, the arrow is decorative). */
export function SortHeader(props: {
  label: string
  /** The sort of this column: none, or the direction. */
  sort: 'none' | 'ascending' | 'descending'
  onSort: () => void
  hideNarrow?: boolean
}) {
  return (
    <th scope="col" className={props.hideNarrow ? 'ui-hide-narrow' : undefined} aria-sort={props.sort}>
      <button type="button" className="ui-sort" onClick={props.onSort}>
        {props.label}
        <span aria-hidden="true">{props.sort === 'ascending' ? ' ↑' : props.sort === 'descending' ? ' ↓' : ''}</span>
      </button>
    </th>
  )
}

/**
 * The name cell of a file row: the kind icon and the name (a link or a button). `meta` is a second
 * line that shows only on the phone, where the other columns are hidden (who changed it, when).
 */
export function NameCell({ icon, children, meta }: { icon: ReactNode; children: ReactNode; meta?: ReactNode }) {
  return (
    <span className="ui-name-cell">
      {icon}
      <span className="ui-name-cell-text">
        {children}
        {meta && <span className="ui-cell-meta">{meta}</span>}
      </span>
    </span>
  )
}

/** A line under the main text of a cell that shows only on the phone (the hidden columns' facts). */
export function NarrowMeta({ children }: { children: ReactNode }) {
  return <span className="ui-cell-meta">{children}</span>
}
