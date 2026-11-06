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

/** "5 Nov 2026, 14:00" in the person's locale (24 h where the locale uses it). */
export function formatDate(iso: string | null | undefined): string {
  if (!iso) return '-'
  const d = new Date(iso)
  return d.toLocaleString(undefined, { year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

/** "5 Nov, 14:00": a short date and time, for versions. */
export function formatShortDate(iso: string): string {
  return new Date(iso).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}

function time(d: Date): string {
  return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
}

/**
 * A relative time under 7 days ("Just now", "5 minutes ago", "2 hours ago", "Yesterday at 14:00",
 * "Tuesday at 09:12"), else the date ("5 Nov 2026"). copy.md section 1, "Numbers".
 */
export function formatWhen(iso: string | null | undefined, now = new Date()): string {
  if (!iso) return '-'
  const d = new Date(iso)
  const s = Math.round((now.getTime() - d.getTime()) / 1000)
  if (s < 0) return formatDate(iso)
  if (s < 60) return 'Just now'
  const m = Math.floor(s / 60)
  if (m < 60) return m === 1 ? '1 minute ago' : `${m} minutes ago`
  const h = Math.floor(m / 60)
  const sameDay = d.toDateString() === now.toDateString()
  if (sameDay) return h === 1 ? '1 hour ago' : `${h} hours ago`
  const yesterday = new Date(now)
  yesterday.setDate(now.getDate() - 1)
  if (d.toDateString() === yesterday.toDateString()) return `Yesterday at ${time(d)}`
  if (s < 7 * 86400) return `${d.toLocaleDateString(undefined, { weekday: 'long' })} at ${time(d)}`
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

/** "1 item", "3 items". */
export function count(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`
}

export const roleLabels: Record<Schemas['Role'], string> = {
  view: 'Can view',
  comment: 'Can comment',
  edit: 'Can edit',
  manage: 'Can manage',
}

/** The one-line meaning of each role (copy.md 3.3). */
export const roleMeanings: Record<Schemas['Role'], string> = {
  view: 'see and download',
  comment: 'see and comment',
  edit: 'change, upload and rename',
  manage: 'also move, delete and share',
}

export const memberRoleLabels: Record<Schemas['MemberRole'], string> = {
  guest: 'Guest',
  member: 'Member',
  admin: 'Admin',
  owner: 'Owner',
}

/** The one-line meaning of each member role (copy.md 3.6). */
export const memberRoleMeanings: Record<Schemas['MemberRole'], string> = {
  owner: 'everything, including billing and deleting the space',
  admin: 'manage members, sharing rules and all agents',
  member: 'work with everything in the team space',
  guest: 'only what is shared with them',
}

const typeNames: [RegExp, string][] = [
  [/^text\/plain/, 'Text file'],
  [/^text\/markdown/, 'Markdown file'],
  [/^text\/csv/, 'Spreadsheet (CSV)'],
  [/^text\/html/, 'Web page'],
  [/^text\//, 'Text file'],
  [/^image\//, 'Picture'],
  [/^video\//, 'Video'],
  [/^audio\//, 'Audio'],
  [/^application\/pdf/, 'PDF document'],
  [/^application\/json/, 'Data file (JSON)'],
  [/xml/, 'Data file (XML)'],
  [/zip|tar|gzip|compressed/, 'Archive'],
  [/spreadsheet|excel/, 'Spreadsheet'],
  [/wordprocessing|msword|opendocument\.text/, 'Document'],
  [/presentation|powerpoint/, 'Presentation'],
]

/** A plain name of a file type ("Text file", "Picture"), never a MIME type. */
export function fileTypeLabel(node: { kind: string; type?: string | null; name: string }): string {
  if (node.kind === 'folder') return 'Folder'
  const t = node.type ?? ''
  for (const [re, label] of typeNames) if (re.test(t)) return label
  const ext = /\.([a-z0-9]{1,8})$/i.exec(node.name)?.[1]
  return ext ? `${ext.toUpperCase()} file` : 'File'
}
