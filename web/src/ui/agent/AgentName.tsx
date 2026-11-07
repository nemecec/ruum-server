import type { ReactNode } from 'react'
import { AgentMark } from '../icon/Icon'
import { VisuallyHidden } from '../text/Text'
import './agent.css'

/**
 * U+2060 WORD JOINER. Chromium breaks a line after an inline SVG even before a word joiner; inside
 * a `nowrap` span with the mark (`.ui-agent-lead`), the joiner keeps the first word on the line.
 */
const JOIN = '\u2060'

/**
 * An agent's name with the agent mark, in the agent colour (design-system 4.10). `srText` is read
 * after the name ("(an agent)"), so a screen reader knows it is an agent. A long name wraps like
 * the text around it, and the mark stays with the first word.
 */
export function AgentName({ children, srText }: { children: ReactNode; srText?: string }) {
  return (
    <span className="ui-agent-name">
      <span className="ui-agent-lead" aria-hidden="true">
        <AgentMark />
        {JOIN}
      </span>
      {children}
      {srText && <VisuallyHidden>{srText}</VisuallyHidden>}
    </span>
  )
}
