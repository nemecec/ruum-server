import type { ReactNode } from 'react'
import { IconButton } from '../button/Button'
import { cx } from '../cx'
import { Icon, type IconName } from '../icon/Icon'
import { VisuallyHidden } from '../text/Text'
import { nearestTagColor } from './tagColor'
import './badge.css'
import { t } from '../../i18n'

export type BadgeTone = 'neutral' | 'success' | 'warning' | 'danger' | 'agent' | 'primary'

/** A short label of state or role (design-system 4.17). Always text, never only colour. */
export function Badge({ tone = 'neutral', children }: { tone?: BadgeTone; children: ReactNode }) {
  return <span className={cx('ui-badge', `ui-badge-${tone}`)}>{children}</span>
}

/** A tag: its name with its colour as a 3 px edge and a dot; optionally removable. */
export function Tag(props: { name: string; color?: string | null; onRemove?: () => void; as?: 'li' | 'span' }) {
  const As = props.as ?? 'li'
  const color = nearestTagColor(props.color)
  return (
    <As className={cx('ui-tag', color && `ui-tag-${color}`)}>
      <span className="ui-tag-dot" aria-hidden="true" />
      {props.name}
      {props.onRemove && <IconButton icon="close" size="sm" label={t('ui.tag.remove', { name: props.name })} onClick={props.onRemove} />}
    </As>
  )
}

/** A row of tags (a `ul`). */
export function TagList({ children, label }: { children: ReactNode; label?: string }) {
  return (
    <ul className="ui-tag-list" aria-label={label}>
      {children}
    </ul>
  )
}

const chipIcons: Record<'hidden' | 'standin' | 'value', IconName> = { hidden: 'lock', standin: 'person', value: 'hash' }

/**
 * A marker that must never look like data (design-system 4b): a hidden value, a stand-in or a
 * query value. A dashed edge, an icon and words.
 */
export function Chip({
  kind,
  children,
  srText,
  title,
}: {
  kind: 'hidden' | 'standin' | 'value'
  children: ReactNode
  srText?: string
  title?: string
}) {
  return (
    <span className="ui-chip" title={title}>
      <Icon name={chipIcons[kind]} small />
      <span aria-hidden={srText ? true : undefined}>{children}</span>
      {srText && <VisuallyHidden>{srText}</VisuallyHidden>}
    </span>
  )
}
