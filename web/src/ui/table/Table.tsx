import type { DragEventHandler, ReactNode, ThHTMLAttributes, TdHTMLAttributes } from 'react'
import { cx } from '../cx'
import { VisuallyHidden } from '../text/Text'
import './table.css'

/** A data table with a caption (visually hidden unless `showCaption`). */
export function Table({ caption, showCaption, children }: { caption: string; showCaption?: boolean; children: ReactNode }) {
  return (
    <div className="ui-table-wrap">
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

/** The name cell of a file row: the kind icon and the name (a link or a button). */
export function NameCell({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <span className="ui-name-cell">
      {icon}
      {children}
    </span>
  )
}
