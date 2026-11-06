import type { Schemas } from '../api/client'

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`
}

export function formatDate(iso: string | null | undefined): string {
  if (!iso) return '-'
  const d = new Date(iso)
  return d.toLocaleString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

/** Names of users the UI knows (the signed-in user), for `on_behalf_of`, which is only an ID. */
const knownNames: Record<string, string> = {}

export function rememberName(id: string, name: string): void {
  knownNames[id] = name
}

/** "Claude Desktop (agent for Ada Lovelace)" for a principal and the user it acts for. */
export function actorText(actor: Schemas['PrincipalRef'], onBehalfOf?: string | null, names: Record<string, string> = knownNames): string {
  const name = actor.display_name ?? names[actor.id] ?? actor.id.slice(0, 8)
  if (actor.kind === 'agent') {
    const user = onBehalfOf ? (names[onBehalfOf] ?? 'a user') : 'a user'
    return `${name} (agent for ${user})`
  }
  if (actor.kind === 'share_link') return `Share link ${name}`
  return name
}

export const roleLabels: Record<Schemas['Role'], string> = {
  view: 'Can view',
  comment: 'Can comment',
  edit: 'Can edit',
  manage: 'Can manage',
}

export const memberRoleLabels: Record<Schemas['MemberRole'], string> = {
  guest: 'Guest',
  member: 'Member',
  admin: 'Admin',
  owner: 'Owner',
}
