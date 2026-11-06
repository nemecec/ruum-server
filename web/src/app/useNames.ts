import { createContext, useContext } from 'react'
import type { Schemas } from '../api/client'

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

/** The text of who did a change: "Ada Lovelace", or "Claude Code for Ada Lovelace" for an agent. */
export function actorLabel(names: Names, actor: Schemas['PrincipalRef'], onBehalfOf?: string | null): { text: string; agent: boolean } {
  switch (actor.kind) {
    case 'agent': {
      const info = names.agent(actor.id)
      const name = actor.display_name ?? info?.name ?? 'An agent'
      const forId = onBehalfOf ?? info?.ownerId
      const person = names.person(forId)?.name
      return { text: person ? `${name} for ${person}` : name, agent: true }
    }
    case 'share_link':
      return { text: 'Anyone with the link', agent: false }
    case 'user':
      return { text: actor.display_name ?? names.person(actor.id)?.name ?? 'Someone', agent: false }
    default:
      return { text: actor.display_name ?? 'An app', agent: false }
  }
}

/** The plain-text form of `Actor`, for labels and toasts. */
export function useActorText() {
  const names = useNames()
  return (pair: { actor: Schemas['PrincipalRef']; on_behalf_of: string | null }) => actorLabel(names, pair.actor, pair.on_behalf_of).text
}
