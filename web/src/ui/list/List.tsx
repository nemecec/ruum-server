import type { HTMLAttributes, LiHTMLAttributes, ReactNode } from 'react'
import { Button } from '../button/Button'
import { cx } from '../cx'
import { ErrorNote } from '../feedback/Feedback'
import { Text } from '../text/Text'
import './list.css'

type ItemProps = Omit<LiHTMLAttributes<HTMLLIElement>, 'className' | 'style'>

/** A list of rows with dividers (sessions, keys, members, activity). An `ol` with `ordered`. */
export function List({
  ordered,
  children,
  ...rest
}: { ordered?: boolean; children: ReactNode } & Omit<HTMLAttributes<HTMLElement>, 'className' | 'style'>) {
  const As = ordered ? 'ol' : 'ul'
  return (
    <As className="ui-list" {...rest}>
      {children}
    </As>
  )
}

/**
 * One row of a List: its parts side by side, wrapping on a narrow screen. `block` stacks them
 * (a version with its actions under it). `undone`: crossed out (an undone change).
 */
export function ListRow({
  block,
  undone,
  current,
  children,
  ...rest
}: { block?: boolean; undone?: boolean; current?: boolean; children: ReactNode } & ItemProps) {
  return (
    <li
      className={cx('ui-list-row', block && 'ui-list-row-block', undone && 'ui-list-row-undone', current && 'ui-list-row-current')}
      {...rest}
    >
      {children}
    </li>
  )
}

/** The text of a row that is crossed out when the row is undone. */
export function ListRowMain({ children }: { children: ReactNode }) {
  return <span className="ui-list-row-main">{children}</span>
}

/** The muted line of a List when it has no rows ("No app passwords."). */
export function ListEmpty({ children }: { children: ReactNode }) {
  return <li className="ui-list-empty">{children}</li>
}

/** Facts as terms and values (a `dl`): the details of an item, a profile. */
export function Facts({ children }: { children: ReactNode }) {
  return <dl className="ui-facts">{children}</dl>
}

export function Fact({ label, children }: { label: ReactNode; children: ReactNode }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{children}</dd>
    </>
  )
}

/** A scrolling list of choices (the folders of the folder picker). */
export function Picker({ label, children }: { label: string; children: ReactNode }) {
  return (
    <ul className="ui-picker" aria-label={label}>
      {children}
    </ul>
  )
}

export function PickerItem({
  icon,
  children,
  onClick,
  disabled,
}: {
  icon?: ReactNode
  children: ReactNode
  onClick: () => void
  disabled?: boolean
}) {
  return (
    <li>
      <button type="button" className="ui-picker-item" disabled={disabled} onClick={onClick}>
        {icon}
        {children}
      </button>
    </li>
  )
}

export function PickerEmpty({ children }: { children: ReactNode }) {
  return <li className="ui-picker-empty">{children}</li>
}

/** "Load more" under a paged list, while the server has more items (`next_cursor`). */
export function LoadMore({
  state,
}: {
  state: { data: { nextCursor: string | null } | undefined; loadMore: () => void; loadingMore: boolean; moreError: unknown }
}) {
  if (!state.data?.nextCursor) return null
  return (
    <div className="ui-load-more">
      <ErrorNote error={state.moreError} />
      <div className="ui-load-more-row">
        <Text as="span" tone="muted" size="sm">
          There are more items.
        </Text>
        <Button disabled={state.loadingMore} busy={state.loadingMore} onClick={state.loadMore}>
          {state.loadingMore ? 'Loading...' : 'Load more'}
        </Button>
      </div>
    </div>
  )
}
