import type { AllHTMLAttributes, ElementType, ReactNode } from 'react'
import { cx } from '../cx'
import './layout.css'

/** A step of the spacing scale (design-system 2.3): 1 = 4 px ... 7 = 48 px. */
export type Space = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7

type BoxProps = Omit<AllHTMLAttributes<HTMLElement>, 'className' | 'style' | 'wrap'> & {
  as?: ElementType
  children?: ReactNode
}

/** Children one under the other, with an even gap. A `ul` or `ol` loses its bullets. */
export function Stack({ as: As = 'div', gap = 3, align, ...rest }: BoxProps & { gap?: Space; align?: 'start' | 'center' | 'stretch' }) {
  return <As className={cx('ui-stack', `ui-gap-${gap}`, align && `ui-align-${align}`)} {...rest} />
}

/** Children side by side; they wrap to the next line when the row is full. */
export function Row({
  as: As = 'div',
  gap = 2,
  align = 'center',
  justify = 'start',
  nowrap,
  ...rest
}: BoxProps & {
  gap?: Space
  align?: 'start' | 'center' | 'end' | 'baseline'
  justify?: 'start' | 'end' | 'between' | 'center'
  nowrap?: boolean
}) {
  return <As className={cx('ui-row', `ui-gap-${gap}`, `ui-align-${align}`, `ui-justify-${justify}`, nowrap && 'ui-nowrap')} {...rest} />
}

/** Takes the free space of a Row. */
export function Grow({ as: As = 'span', ...rest }: BoxProps) {
  return <As className="ui-grow" {...rest} />
}

/** The buttons of a form, a dialog or a card: at the end by default (the main action last). */
export function Actions({ align = 'end', as: As = 'div', ...rest }: BoxProps & { align?: 'start' | 'end' | 'center' }) {
  return <As className={cx('ui-actions', `ui-justify-${align}`)} {...rest} />
}

/** A title and its actions on one line, for page and panel headers. */
export function Toolbar({ as: As = 'div', ...rest }: BoxProps) {
  return <As className="ui-toolbar" {...rest} />
}

/** A grid of equal columns that fall to one column on a narrow screen. */
export function Columns({ as: As = 'div', ...rest }: BoxProps) {
  return <As className="ui-columns" {...rest} />
}

/** Indents what belongs to the control above (the detail of a radio choice). */
export function Indent({ as: As = 'div', ...rest }: BoxProps) {
  return <As className="ui-indent" {...rest} />
}

/** A page outside the app shell (sign-in, an invitation): centred, narrow or default width. */
export function Page({ as: As = 'main', narrow, ...rest }: BoxProps & { narrow?: boolean }) {
  return <As className={cx('ui-page', narrow && 'ui-page-narrow')} {...rest} />
}

/** The content of a page inside the shell: its blocks one under the other. */
export function PageBody({ as: As = 'div', narrow, ...rest }: BoxProps & { narrow?: boolean }) {
  return <As className={cx('ui-page-body', narrow && 'ui-page-body-narrow')} {...rest} />
}

/** A card: a surface with an edge, for one topic of a page. A `section` by default. */
export function Card({ as: As = 'section', ...rest }: BoxProps) {
  return <As className="ui-card" {...rest} />
}
