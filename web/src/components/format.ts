// Dates, times, numbers, sizes and labels in the locale of the UI (design 12.4). The numbers and
// dates come from `Intl` with the current locale; the words come from the catalogue.
import type { Schemas } from '../api/client'
import { currentIntl, labels, t, type MessageKey } from '../i18n'

const sizeUnits = ['format.size.kb', 'format.size.mb', 'format.size.gb', 'format.size.tb'] as const

/** "512 B", "1.5 MB", "12 GB" (one decimal under 10). */
export function formatBytes(n: number): string {
  const intl = currentIntl()
  if (n < 1024) return t('format.size.b', { n: intl.formatNumber(n) })
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < sizeUnits.length - 1) {
    v /= 1024
    i++
  }
  const digits = v < 10 ? 1 : 0
  const value = intl.formatNumber(digits ? v : Math.round(v), { minimumFractionDigits: digits, maximumFractionDigits: digits })
  return t(sizeUnits[i], { n: value })
}

/** A number in the locale ("1,234" or "1 234"). */
export function formatNumber(n: number): string {
  return currentIntl().formatNumber(n)
}

/** "5 Nov 2026, 14:00" in the person's locale (24 h where the locale uses it, "2:00 PM" else). */
export function formatDate(iso: string | null | undefined): string {
  if (!iso) return '-'
  return currentIntl().formatDate(new Date(iso), { year: 'numeric', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })
}

/** "5 Nov 2026": a date without the time. */
export function formatDay(iso: string | Date): string {
  return currentIntl().formatDate(new Date(iso), { year: 'numeric', month: 'short', day: 'numeric' })
}

/** "5 Nov, 14:00": a short date and time, for versions. */
export function formatShortDate(iso: string): string {
  return currentIntl().formatDate(new Date(iso), { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })
}

/** "14:00" (or "2:00 PM"; never "02:00 PM"). */
export function formatTime(d: Date | string): string {
  return currentIntl().formatTime(new Date(d), { hour: 'numeric', minute: '2-digit' })
}

/**
 * A relative time under 7 days ("Just now", "5 minutes ago", "2 hours ago", "Yesterday at 14:00",
 * "Tuesday at 09:12"), else the date ("5 Nov 2026"). copy.md section 1, "Numbers".
 */
export function formatWhen(iso: string | null | undefined, now = new Date()): string {
  if (!iso) return '-'
  return whenParts(iso, now).text
}

/**
 * `formatWhen` for the middle of a sentence ("last used yesterday at 2:00 PM"). The case of the
 * words is in the catalogue (`format.when.*.inline`); code never changes the case of a message.
 */
export function formatWhenInline(iso: string | null | undefined, now = new Date()): string {
  if (!iso) return '-'
  return whenText(iso, now, true)
}

/**
 * `formatWhen` and the kind of its text: `recent` ("5 minutes ago", today), `day` ("Yesterday at
 * 14:00", a weekday with the time) or `date` (no time of day).
 */
export function whenParts(iso: string, now = new Date()): { text: string; kind: 'recent' | 'day' | 'date' } {
  const kind = whenKind(iso, now)
  return { text: whenText(iso, now), kind }
}

function whenKind(iso: string, now: Date): 'recent' | 'day' | 'date' {
  const d = new Date(iso)
  const s = Math.round((now.getTime() - d.getTime()) / 1000)
  if (s < 0) return 'date'
  if (s < 3600 || d.toDateString() === now.toDateString()) return 'recent'
  return s < 7 * 86400 ? 'day' : 'date'
}

function whenText(iso: string, now: Date, inline = false): string {
  const d = new Date(iso)
  const s = Math.round((now.getTime() - d.getTime()) / 1000)
  if (s < 0) return formatDate(iso)
  if (s < 60) return inline ? t('format.when.now.inline') : t('format.when.now')
  const m = Math.floor(s / 60)
  if (m < 60) return t('format.when.minutes', { n: m })
  const h = Math.floor(m / 60)
  const sameDay = d.toDateString() === now.toDateString()
  if (sameDay) return t('format.when.hours', { n: h })
  const yesterday = new Date(now)
  yesterday.setDate(now.getDate() - 1)
  if (d.toDateString() === yesterday.toDateString())
    return inline ? t('format.when.yesterday.inline', { time: formatTime(d) }) : t('format.when.yesterday', { time: formatTime(d) })
  if (s < 7 * 86400) return t('format.when.weekday', { day: currentIntl().formatDate(d, { weekday: 'long' }), time: formatTime(d) })
  return formatDay(d)
}

export const roleLabels: Record<Schemas['Role'], string> = labels({
  view: 'role.view',
  comment: 'role.comment',
  edit: 'role.edit',
  manage: 'role.manage',
})

/** The role inside a sentence ("Changed Liis to can edit."): the case is in the catalogue. */
export const roleInline: Record<Schemas['Role'], string> = labels({
  view: 'role.view.inline',
  comment: 'role.comment.inline',
  edit: 'role.edit.inline',
  manage: 'role.manage.inline',
})

/** The one-line meaning of each role (copy.md 3.3). */
export const roleMeanings: Record<Schemas['Role'], string> = labels({
  view: 'role.view.meaning',
  comment: 'role.comment.meaning',
  edit: 'role.edit.meaning',
  manage: 'role.manage.meaning',
})

export const memberRoleLabels: Record<Schemas['MemberRole'], string> = labels({
  guest: 'memberRole.guest',
  member: 'memberRole.member',
  admin: 'memberRole.admin',
  owner: 'memberRole.owner',
})

/** The member role inside a sentence ("Liis is now an admin."): the case is in the catalogue. */
export const memberRoleInline: Record<Schemas['MemberRole'], string> = labels({
  guest: 'memberRole.guest.inline',
  member: 'memberRole.member.inline',
  admin: 'memberRole.admin.inline',
  owner: 'memberRole.owner.inline',
})

/** The one-line meaning of each member role (copy.md 3.6). */
export const memberRoleMeanings: Record<Schemas['MemberRole'], string> = labels({
  owner: 'memberRole.owner.meaning',
  admin: 'memberRole.admin.meaning',
  member: 'memberRole.member.meaning',
  guest: 'memberRole.guest.meaning',
})

const typeNames: [RegExp, MessageKey][] = [
  [/^text\/plain/, 'fileType.text'],
  [/^text\/markdown/, 'fileType.markdown'],
  [/^text\/csv/, 'fileType.csv'],
  [/^text\/html/, 'fileType.html'],
  [/^text\//, 'fileType.text'],
  [/^image\//, 'fileType.image'],
  [/^video\//, 'fileType.video'],
  [/^audio\//, 'fileType.audio'],
  [/^application\/pdf/, 'fileType.pdf'],
  [/^application\/json/, 'fileType.json'],
  [/xml/, 'fileType.xml'],
  [/zip|tar|gzip|compressed/, 'fileType.archive'],
  [/spreadsheet|excel/, 'fileType.spreadsheet'],
  [/wordprocessing|msword|opendocument\.text/, 'fileType.document'],
  [/presentation|powerpoint/, 'fileType.presentation'],
]

/** A plain name of a file type ("Text file", "Picture"), never a MIME type. */
export function fileTypeLabel(node: { kind: string; type?: string | null; name: string }): string {
  if (node.kind === 'folder') return t('fileType.folder')
  const type = node.type ?? ''
  for (const [re, key] of typeNames) if (re.test(type)) return t(key)
  const ext = /\.([a-z0-9]{1,8})$/i.exec(node.name)?.[1]
  // i18n-ignore: a file extension, shown as written in capitals
  return ext ? t('fileType.extension', { ext: ext.toUpperCase() }) : t('fileType.file')
}
