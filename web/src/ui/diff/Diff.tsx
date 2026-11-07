import type { ReactNode } from 'react'
import { VisuallyHidden } from '../text/Text'
import './diff.css'

/**
 * The changes between two versions (design-system 4.9). Insertions and deletions use `<ins>` and
 * `<del>` with hidden "Added:" and "Removed:" words, so colour is never the only cue.
 */
export function DiffView({ busy, children }: { busy?: boolean; children: ReactNode }) {
  return (
    <div className="ui-diff" aria-label="Changes" role="group" aria-busy={busy}>
      {children}
    </div>
  )
}

export function DiffList({ children }: { children: ReactNode }) {
  return <ul className="ui-diff-list">{children}</ul>
}

/** One change: its kind in words, where, the old and new values, and nested changes. */
export function DiffChange(props: {
  kind: string
  where?: string | null
  old?: string
  new?: string
  format?: string | null
  children?: ReactNode
}) {
  const values = props.old !== undefined || props.new !== undefined || props.format
  return (
    <li className="ui-diff-change">
      <span className="ui-diff-kind">{props.kind}</span>
      {props.where && <span className="ui-diff-where"> {props.where}</span>}
      {values && (
        <div className="ui-diff-values">
          {props.old !== undefined && (
            <del>
              <VisuallyHidden>Removed: </VisuallyHidden>
              {props.old}
            </del>
          )}
          {props.new !== undefined && (
            <ins>
              <VisuallyHidden>Added: </VisuallyHidden>
              {props.new}
            </ins>
          )}
          {props.format && <span className="ui-diff-format">{props.format}</span>}
        </div>
      )}
      {props.children}
    </li>
  )
}
