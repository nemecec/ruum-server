import { createContext, useContext } from 'react'
import type { Schemas } from '../api/client'

export type Session = {
  me: Schemas['Me']
  tenants: Schemas['TenantMembership'][]
  reloadTenants: () => void
}

export const SessionContext = createContext<Session | null>(null)

/** The signed-in user and the tenants. Only inside `RequireSession`. */
export function useSession(): Session {
  const s = useContext(SessionContext)
  if (!s) throw new Error('useSession outside RequireSession')
  return s
}
