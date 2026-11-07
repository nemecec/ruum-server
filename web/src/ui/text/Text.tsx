import type { AllHTMLAttributes, ElementType, HTMLAttributes, ReactNode } from 'react'
import { cx } from '../cx'
import './text.css'

export type Tone = 'default' | 'muted' | 'faint' | 'danger' | 'success' | 'warning' | 'agent' | 'primary'

type TextProps = Omit<AllHTMLAttributes<HTMLElement>, 'className' | 'style' | 'size'> & {
  as?: ElementType
  tone?: Tone
  size?: 'xs' | 'sm' | 'md' | 'lg'
  weight?: 'regular' | 'strong'
  /** A span that takes its own line. */
  block?: boolean
  mono?: boolean
  italic?: boolean
  /** Numbers in columns (tabular figures). */
  numeric?: boolean
  /** One line, cut with an ellipsis. */
  truncate?: boolean
  /** Hidden on the phone (an extra detail there). */
  hideNarrow?: boolean
  children?: ReactNode
}

/** Running text. A paragraph by default; `as="span"` inside a line. */
export function Text({
  as: As = 'p',
  tone = 'default',
  size,
  weight,
  block,
  mono,
  italic,
  numeric,
  truncate,
  hideNarrow,
  ...rest
}: TextProps) {
  return (
    <As
      className={cx(
        'ui-text',
        tone !== 'default' && `ui-tone-${tone}`,
        size && `ui-size-${size}`,
        weight && `ui-weight-${weight}`,
        block && 'ui-block',
        mono && 'ui-mono',
        italic && 'ui-italic',
        numeric && 'ui-numeric',
        truncate && 'ui-truncate',
        hideNarrow && 'ui-text-hide-narrow',
      )}
      {...rest}
    />
  )
}

/** Text for screen readers only (a table caption, "(an agent)"). */
export function VisuallyHidden({
  as: As = 'span',
  ...rest
}: Omit<AllHTMLAttributes<HTMLElement>, 'className' | 'style'> & { as?: ElementType }) {
  return <As className="ui-visually-hidden" {...rest} />
}

/** Code, a key prefix or a command, in the mono face. */
export function Code({ children }: { children: ReactNode }) {
  return <code className="ui-code">{children}</code>
}

/** A list with bullets (or numbers) inside running text: steps, hints, names. */
export function ProseList({
  ordered,
  size,
  children,
  ...rest
}: { ordered?: boolean; size?: 'sm' | 'md'; children: ReactNode } & Omit<HTMLAttributes<HTMLElement>, 'className' | 'style'>) {
  const As = ordered ? 'ol' : 'ul'
  return (
    <As className={cx('ui-prose-list', size && `ui-size-${size}`)} {...rest}>
      {children}
    </As>
  )
}
