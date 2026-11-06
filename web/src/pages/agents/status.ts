import type { Schemas } from '../../api/client'

export function agentStatus(a: Schemas['Agent']): string {
  if (a.disabled_at) return a.disabled_reason === 'by_admin' ? 'Disabled by an admin' : a.disabled_reason === 'owner_left' ? 'Disabled (owner left)' : 'Disabled'
  if (a.scope.expires_at && a.scope.expires_at < new Date().toISOString()) return 'Expired'
  return 'Active'
}
