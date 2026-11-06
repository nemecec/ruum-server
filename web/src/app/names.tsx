// Names of people and agents for the IDs in API answers. The API's `PrincipalRef.display_name` and
// the grantee's `display_name` are optional ("filled when known"); until the server fills them
// everywhere (docs/ux/gap-analysis.md, contract need C-7), the UI looks the IDs up in what the
// person may list: the members of the space (members and up) and the agents (their own; admins:
// all). The UI never shows an ID in their place.

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { client, unwrap, type Schemas } from '../api/client'
import { actorLabel, NamesContext, useNames, type AgentInfo, type Names, type Person } from './useNames'
import { AgentGlyph } from '../components/ui'

/** Loads the names of a space: members (if the person may list them) and agents. */
export function NamesProvider(props: {
  tenantId: string | null
  canListMembers: boolean
  me: { id: string; name: string; email: string }
  children: ReactNode
}) {
  const { tenantId, canListMembers, me } = props
  const [people, setPeople] = useState<Record<string, Person>>({})
  const [agents, setAgents] = useState<Record<string, AgentInfo>>({})
  // An ID that is not in the lists (an agent or a member added since they loaded) loads them
  // again, once per ID.
  const [tick, setTick] = useState(0)
  const asked = useRef(new Set<string>())
  const ask = useCallback((id: string) => {
    if (asked.current.has(id) || asked.current.size > 200) return
    asked.current.add(id)
    setTimeout(() => setTick((t) => t + 1), 0)
  }, [])

  useEffect(() => {
    if (!tenantId) return
    let active = true
    const header = { 'Ruum-Tenant': tenantId }
    if (canListMembers) {
      void collect((cursor) =>
        unwrap(client.GET('/v1/tenants/{tenant_id}/members', { params: { path: { tenant_id: tenantId }, query: cursor ? { cursor } : {} } })),
      )
        .then((items) => {
          if (active) setPeople((p) => ({ ...p, ...Object.fromEntries(items.map((m) => [m.user_id, { name: m.display_name, email: m.email }])) }))
        })
        .catch(() => {})
    }
    void collect((cursor) => unwrap(client.GET('/v1/agents', { params: { query: cursor ? { cursor } : {}, header } })))
      .then((items) => {
        if (active) setAgents(Object.fromEntries(items.map((a) => [a.id, { name: a.name, ownerId: a.owner_id }])))
      })
      .catch(() => {})
    return () => {
      active = false
    }
  }, [tenantId, canListMembers, tick])

  const learn = useCallback((id: string, person: Person) => {
    setPeople((p) => (p[id]?.name === person.name && p[id]?.email === person.email ? p : { ...p, [id]: { ...p[id], ...person } }))
  }, [])

  const value = useMemo<Names>(
    () => ({
      person: (id) => {
        if (!id) return undefined
        if (id === me.id) return { name: me.name, email: me.email }
        if (!people[id] && canListMembers) ask(id)
        return people[id]
      },
      agent: (id) => {
        if (!agents[id]) ask(id)
        return agents[id]
      },
      learn,
    }),
    [people, agents, me.id, me.name, me.email, learn, ask, canListMembers],
  )
  return <NamesContext.Provider value={value}>{props.children}</NamesContext.Provider>
}

async function collect<T>(load: (cursor: string | undefined) => Promise<{ items: T[]; next_cursor?: string | null }>): Promise<T[]> {
  const all: T[] = []
  let cursor: string | undefined
  for (let i = 0; i < 10; i++) {
    const page = await load(cursor)
    all.push(...page.items)
    if (!page.next_cursor) break
    cursor = page.next_cursor
  }
  return all
}

/** Who did a change, with the agent mark for agents ("Claude Code for Ann", an agent for Ann). */
export function Actor({ pair }: { pair: Schemas['ActorPair'] | { actor: Schemas['PrincipalRef']; on_behalf_of: string | null } }) {
  const names = useNames()
  const { text, agent } = actorLabel(names, pair.actor, pair.on_behalf_of)
  if (!agent) return <span>{text}</span>
  return (
    <span className="agent-name">
      <AgentGlyph />
      {text}
      <span className="sr-only"> (an agent)</span>
    </span>
  )
}

