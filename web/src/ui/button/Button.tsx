import { forwardRef, type AnchorHTMLAttributes, type ButtonHTMLAttributes, type ReactNode } from 'react'
import { Link, type LinkProps } from 'react-router'
import { cx } from '../cx'
import { Spinner } from '../feedback/Feedback'
import { Icon, type IconName } from '../icon/Icon'
import './button.css'

/**
 * The kinds of design-system 4.16. `primary`: one per view. `secondary`: everything else.
 * `quiet`: low-weight actions in rows. `danger`: only the confirm of an irreversible action.
 * `link`: an action that reads as a link inside text (a file name, "Clear selection").
 */
export type ButtonVariant = 'primary' | 'secondary' | 'quiet' | 'danger' | 'link'

type Common = {
  variant?: ButtonVariant
  /** `sm`: compact buttons in rows and panels. */
  size?: 'sm' | 'md'
  /** Busy: a spinner before the label, `aria-busy`; the label stays. */
  busy?: boolean
  icon?: IconName
  /** Fills the width of its column. */
  wide?: boolean
}

function classes({ variant = 'secondary', size = 'md', wide }: Common) {
  return cx('ui-button', `ui-button-${variant}`, size === 'sm' && 'ui-button-sm', wide && 'ui-button-wide')
}

function content(busy: boolean | undefined, icon: IconName | undefined, children: ReactNode) {
  return (
    <>
      {busy ? <Spinner /> : icon ? <Icon name={icon} /> : null}
      {children}
    </>
  )
}

export type ButtonProps = Common & Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'className' | 'style'>

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant, size, busy, icon, wide, type = 'button', children, ...rest },
  ref,
) {
  return (
    <button ref={ref} type={type} className={classes({ variant, size, wide })} aria-busy={busy || undefined} {...rest}>
      {content(busy, icon, children)}
    </button>
  )
})

/** A link in the app (React Router) that looks like a button. */
export function ButtonLink({ variant, size, icon, wide, children, ...rest }: Common & Omit<LinkProps, 'className' | 'style'>) {
  return (
    <Link className={classes({ variant, size, wide })} {...rest}>
      {content(false, icon, children)}
    </Link>
  )
}

/** A plain anchor (a full page load, an outside address) that looks like a button. */
export function ButtonAnchor({
  variant,
  size,
  icon,
  wide,
  children,
  ...rest
}: Common & Omit<AnchorHTMLAttributes<HTMLAnchorElement>, 'className' | 'style'>) {
  return (
    <a className={classes({ variant, size, wide })} {...rest}>
      {content(false, icon, children)}
    </a>
  )
}

/**
 * A button with only an icon: `label` is its accessible name and its tooltip (2.5.3 Label in
 * Name). 36 px, 44 px on touch screens.
 */
export const IconButton = forwardRef<
  HTMLButtonElement,
  { icon: IconName; label: string; size?: 'sm' | 'md' } & Omit<
    ButtonHTMLAttributes<HTMLButtonElement>,
    'className' | 'style' | 'children' | 'aria-label'
  >
>(function IconButton({ icon, label, size = 'md', type = 'button', title, ...rest }, ref) {
  return (
    <button
      ref={ref}
      type={type}
      className={cx('ui-icon-button', size === 'sm' && 'ui-icon-button-sm')}
      aria-label={label}
      title={title ?? label}
      {...rest}
    >
      <Icon name={icon} />
    </button>
  )
})
