import type { Schemas } from '../../api/client'
import { formatDay, formatWhenInline, roleLabels } from '../../components/format'
import { t } from '../../i18n'

/** The status of an agent in plain words (copy.md 3.5). */
export function agentStatus(a: Schemas['Agent']): string {
  if (a.disabled_at)
    return a.disabled_reason === 'by_admin'
      ? t('agent.status.pausedByAdmin')
      : a.disabled_reason === 'owner_left'
        ? t('agent.status.ownerLeft')
        : t('agent.status.paused')
  if (a.scope.expires_at && a.scope.expires_at < new Date().toISOString()) return t('agent.status.ended')
  if (!a.last_used_at) return t('agent.status.unused')
  return t('agent.status.active', { when: formatWhenInline(a.last_used_at) })
}

/** "Can edit in folder Clients, until 4 Feb 2027" (copy.md 3.5, access summary). */
export function accessSummary(scope: Schemas['AgentScope'], folderName: string | null, spaceName: string | null): string {
  // One message with a select per case: a fallback noun phrase in a slot that needs a case form
  // breaks Estonian (i18n.md).
  return t('agent.summary', {
    role: roleLabels[scope.max_role],
    where: scope.subtree ? (folderName ? 'folder' : 'someFolder') : spaceName ? 'everywhere' : 'everywhereHere',
    folder: folderName ?? '',
    space: spaceName ?? '',
    hasEnd: scope.expires_at ? 'yes' : 'no',
    date: scope.expires_at ? formatDay(scope.expires_at) : '',
    hasBudget: scope.budget?.max_writes_per_hour ? 'yes' : 'no',
    budget: scope.budget?.max_writes_per_hour ?? 0,
  })
}
