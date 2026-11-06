import type { Schemas } from '../../api/client'
import { formatWhen } from '../../components/format'

/** The status of an agent in plain words (copy.md 3.5). */
export function agentStatus(a: Schemas['Agent']): string {
  if (a.disabled_at) return a.disabled_reason === 'by_admin' ? 'Paused by an admin' : a.disabled_reason === 'owner_left' ? 'Paused (its person left)' : 'Paused'
  if (a.scope.expires_at && a.scope.expires_at < new Date().toISOString()) return 'Access ended'
  if (!a.last_used_at) return 'Not used yet'
  return `Active, last used ${formatWhen(a.last_used_at).toLowerCase()}`
}

/** "Can edit in folder Clients, until 4 Feb 2027" (copy.md 3.5, access summary). */
export function accessSummary(scope: Schemas['AgentScope'], folderName: string | null, spaceName: string): string {
  const role = { view: 'Can view', comment: 'Can comment', edit: 'Can edit', manage: 'Can manage' }[scope.max_role]
  const where = scope.subtree ? `in folder ${folderName ?? 'a folder'}` : `everywhere you have access in ${spaceName}`
  const until = scope.expires_at
    ? `until ${new Date(scope.expires_at).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })}`
    : 'no end date'
  const budget = scope.budget?.max_writes_per_hour ? `, at most ${scope.budget.max_writes_per_hour} changes per hour` : ''
  return `${role} ${where}, ${until}${budget}`
}
