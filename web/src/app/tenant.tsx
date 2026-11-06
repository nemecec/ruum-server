import { createContext, useContext } from 'react'
import type { Schemas } from '../api/client'

export type TenantInfo = {
  tenant: Schemas['Tenant']
  role: Schemas['MemberRole']
  isAdmin: boolean
  /** False for a space that is not in the person's list (only shared items of it are visible). */
  listed?: boolean
}

export const TenantContext = createContext<TenantInfo | null>(null)

export function useTenant(): TenantInfo {
  const t = useContext(TenantContext)
  if (!t) throw new Error('useTenant outside a tenant route')
  return t
}

const LAST_TENANT = 'ruum.lastTenant'

export function rememberTenant(id: string): void {
  try {
    localStorage.setItem(LAST_TENANT, id)
  } catch {
    // Storage can be off (private mode); the home tenant is the fallback.
  }
}

export function lastTenant(): string | null {
  try {
    return localStorage.getItem(LAST_TENANT)
  } catch {
    return null
  }
}

/** The path of a page in a tenant, for example `tenantPath(id, 'files')`. */
export function tenantPath(tenantId: string, ...parts: string[]): string {
  return ['/t', tenantId, ...parts].join('/')
}
