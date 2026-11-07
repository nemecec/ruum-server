import type { ReactNode } from 'react'
import { AgentMark } from '../icon/Icon'
import { VisuallyHidden } from '../text/Text'
import './agent.css'

/**
 * An agent's name with the agent mark, in the agent colour (design-system 4.10). `srText` is read
 * after the name ("(an agent)"), so a screen reader knows it is an agent.
 */
export function AgentName({ children, srText }: { children: ReactNode; srText?: string }) {
  return (
    <span className="ui-agent-name">
      <AgentMark />
      {children}
      {srText && <VisuallyHidden>{srText}</VisuallyHidden>}
    </span>
  )
}
