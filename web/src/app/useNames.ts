import { createContext, useContext } from 'react'
import type { Schemas } from '../api/client'
import { t } from '../i18n'

export type Person = { name: string; email?: string }
export type AgentInfo = { name: string; ownerId: string }

export type Names = {
  person: (id: string | null | undefined) => Person | undefined
  agent: (id: string) => AgentInfo | undefined
  /** Adds what an answer told about a person (for example a grantee with a name or email). */
  learn: (id: string, person: Person) => void
}

export const empty: Names = { person: () => undefined, agent: () => undefined, learn: () => {} }
export const NamesContext = createContext<Names>(empty)

export function useNames(): Names {
  return useContext(NamesContext)
}

/** The text of who did a change: "Ada Lovelace", or "Claude Code (agent of Ada Lovelace)" for an
 * agent. Agent names come from the client and can look like a person's name, so an agent always
 * carries the agent mark and the name of its user (decision 0003-5). The server's names
 * (`display_name`, `on_behalf_of_display_name`) come first; the lists of the space are the
 * fallback for older servers. */
export function actorLabel(
  names: Names,
  actor: Schemas['PrincipalRef'],
  onBehalfOf?: string | null,
  onBehalfOfName?: string | null,
): { text: string; agent: boolean } {
  switch (actor.kind) {
    case 'agent': {
      const info = names.agent(actor.id)
      const name = actor.display_name ?? info?.name ?? t('actor.anAgent')
      const forId = onBehalfOf ?? info?.ownerId
      const person = onBehalfOfName ?? names.person(forId)?.name
      return { text: person ? t('actor.agentOf', { name, person }) : name, agent: true }
    }
    case 'share_link':
      return { text: t('actor.link'), agent: false }
    case 'user':
      return { text: actor.display_name ?? names.person(actor.id)?.name ?? t('actor.someone'), agent: false }
    default:
      return { text: actor.display_name ?? t('actor.app'), agent: false }
  }
}

/** Who did a change, as the API sends it (`ActorPair`, `Version`, `AuditEntry`, ...). */
export type ActorLike = {
  actor: Schemas['PrincipalRef']
  on_behalf_of: string | null
  on_behalf_of_display_name?: string | null
}

/** The plain-text form of `Actor`, for labels and toasts. */
export function useActorText() {
  const names = useNames()
  return (pair: ActorLike) => actorLabel(names, pair.actor, pair.on_behalf_of, pair.on_behalf_of_display_name).text
}
