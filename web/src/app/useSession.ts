import { createContext, useContext } from 'react'
import type { Schemas } from '../api/client'

export type Session = {
  me: Schemas['Me']
  tenants: Schemas['TenantMembership'][]
  /** Loads the spaces again (after a new team space or an accepted invitation). */
  reloadTenants: () => Promise<Schemas['TenantMembership'][]>
}

export const SessionContext = createContext<Session | null>(null)

/** The signed-in user and the spaces. Only inside `RequireSession`. */
export function useSession(): Session {
  const s = useContext(SessionContext)
  if (!s) throw new Error('useSession outside RequireSession')
  return s
}
