import type { Schemas } from '../../api/client'
import { memberRoleLabels } from '../../components/format'
import { t, type MessageKey } from '../../i18n'

/** Readable names of the activity log's actions (ia.md: "Activity log"; never the dotted name). */
const actions: Record<string, MessageKey> = {
  'auth.login': 'audit.action.auth_login',
  'auth.login_failed': 'audit.action.auth_login_failed',
  'auth.logout': 'audit.action.auth_logout',
  'auth.app_password_created': 'audit.action.auth_app_password_created',
  'auth.app_password_revoked': 'audit.action.auth_app_password_revoked',
  'auth.app_password_used': 'audit.action.auth_app_password_used',
  'app_password.created': 'audit.action.app_password_created',
  'app_password.revoked': 'audit.action.app_password_revoked',
  'agent.registered': 'audit.action.agent_registered',
  'agent.updated': 'audit.action.agent_updated',
  'agent.disabled': 'audit.action.agent_disabled',
  'agent.enabled': 'audit.action.agent_enabled',
  'agent.key_created': 'audit.action.agent_key_created',
  'agent.key_revoked': 'audit.action.agent_key_revoked',
  'agent.key_used': 'audit.action.agent_key_used',
  'agent.key_refused': 'audit.action.agent_key_refused',
  'agent.oauth_approved': 'audit.action.agent_oauth_approved',
  'agent.oauth_denied': 'audit.action.agent_oauth_denied',
  'agent.token_issued': 'audit.action.agent_token_issued',
  'agent.undo': 'audit.action.agent_undo',
  'email_grants.bound': 'audit.action.email_grants_bound',
  'grant.added': 'audit.action.grant_added',
  'grant.updated': 'audit.action.grant_updated',
  'grant.removed': 'audit.action.grant_removed',
  'invitation.created': 'audit.action.invitation_created',
  'invitation.accepted': 'audit.action.invitation_accepted',
  'invitation.revoked': 'audit.action.invitation_revoked',
  'member.added': 'audit.action.member_added',
  'member.updated': 'audit.action.member_updated',
  'member.role_changed': 'audit.action.member_role_changed',
  'member.removed': 'audit.action.member_removed',
  'node.created': 'audit.action.node_created',
  'node.copied': 'audit.action.node_copied',
  'node.moved': 'audit.action.node_moved',
  'node.renamed': 'audit.action.node_renamed',
  'node.trashed': 'audit.action.node_trashed',
  'node.restored': 'audit.action.node_restored',
  'node.purged': 'audit.action.node_purged',
  'node.exploded': 'audit.action.node_exploded',
  'node.sharing_updated': 'audit.action.node_sharing_updated',
  'share_link.created': 'audit.action.share_link_created',
  'share_link.revoked': 'audit.action.share_link_revoked',
  'tag.attached': 'audit.action.tag_attached',
  'tag.detached': 'audit.action.tag_detached',
  'tenant.created': 'audit.action.tenant_created',
  'tenant.updated': 'audit.action.tenant_updated',
  'version.created': 'audit.action.version_created',
  'version.labeled': 'audit.action.version_labeled',
  'version.restored': 'audit.action.version_restored',
}

const str = (v: unknown) => (typeof v === 'string' && v ? v : null)

/** The actions whose words take the object right after them ("moved Plan.pdf"); the others take a colon. */
const joined = new Set([
  'node.created',
  'node.copied',
  'node.moved',
  'node.renamed',
  'node.exploded',
  'node.sharing_updated',
  'share_link.created',
  'share_link.revoked',
  'tag.attached',
  'tag.detached',
  'version.created',
  'version.labeled',
  'version.restored',
])

/** "moved Plan.pdf", "invited ann@example.com": the action and its object in plain words. */
export function auditText(a: Schemas['AuditEntry']): string {
  const key = actions[a.action]
  const verb = key ? t(key) : a.action.replace(/[._]/g, ' ')
  const d = a.data ?? {}
  const object =
    str(d.name) ?? str(d.new_name) ?? str(d.node_name) ?? str(d.email) ?? str(d.agent_name) ?? str(d.tag_name) ?? str(d.label) ?? null
  const joins = joined.has(a.action)
  if (object) return t(joins ? 'audit.text.joined' : 'audit.text.colon', { verb, object })
  if (a.target_type === 'node') return t(joins ? 'audit.text.joined' : 'audit.text.colon', { verb, object: t('audit.anItem') })
  return verb
}

/** What the activity log knows about the items and people an entry names. */
export type AuditContext = {
  /** The name of a user, or `undefined` when unknown. */
  person: (id: string) => string | undefined
  /** The name of the space (for a grant to everyone in it). */
  spaceName: string
}

/** A detailed entry: a message of the catalogue, whose `{item}` the row fills with a link to the item. */
export type AuditDetail = { key: MessageKey; values: Record<string, string>; item: string | null }

const roleWords: Record<string, MessageKey> = {
  view: 'audit.role.view',
  comment: 'audit.role.comment',
  edit: 'audit.role.edit',
  manage: 'audit.role.manage',
}
const ruleWords = (v: unknown) => t(v === true ? 'audit.rule.yes' : v === false ? 'audit.rule.no' : 'audit.rule.inherit')

function granteeText(g: unknown, ctx: AuditContext): string | null {
  if (!g || typeof g !== 'object') return null
  const x = g as Record<string, unknown>
  switch (x.type) {
    case 'user':
      return (typeof x.id === 'string' ? ctx.person(x.id) : undefined) ?? str(x.display_name) ?? str(x.email) ?? t('audit.aPerson')
    case 'email':
      return str(x.email)
    case 'tenant_members':
      return t('audit.everyone', { space: ctx.spaceName })
    default:
      return null
  }
}

/**
 * The detailed form of the entries an admin needs to answer "who gave whom access to what":
 * access, role and sharing rule changes name the item, the person and the old and new values.
 * `null` for other entries (they use `auditText`). The item is a separate value so the row can
 * link to it.
 */
export function auditDetail(a: Schemas['AuditEntry'], ctx: AuditContext): AuditDetail | null {
  const d = (a.data ?? {}) as Record<string, unknown>
  const item = a.target_id ?? ''
  const role = (v: unknown) => (typeof v === 'string' ? (roleWords[v] ? t(roleWords[v]) : v) : t('audit.role.some'))
  const memberRole = (v: unknown) =>
    typeof v === 'string' ? ((memberRoleLabels as Record<string, string>)[v] ?? v) : null
  switch (a.action) {
    case 'grant.added': {
      const who = granteeText(d.grantee, ctx)
      if (!who || a.target_type !== 'node') return null
      return { key: 'audit.grant.added', values: { who, role: role(d.role) }, item }
    }
    case 'grant.updated': {
      const who = granteeText(d.grantee, ctx)
      if (!who || a.target_type !== 'node') return null
      return { key: 'audit.grant.updated', values: { who, old: role(d.old_role), new: role(d.new_role) }, item }
    }
    case 'grant.removed': {
      const who = granteeText(d.grantee, ctx)
      if (!who || a.target_type !== 'node') return null
      return { key: 'audit.grant.removed', values: { who, role: role(d.role) }, item }
    }
    case 'node.sharing_updated':
      if (a.target_type !== 'node') return null
      return {
        key: 'audit.sharing',
        values: { old: ruleWords(d.old_editors_can_share), new: ruleWords(d.new_editors_can_share) },
        item,
      }
    case 'member.role_changed': {
      const who = (a.target_id && ctx.person(a.target_id)) ?? t('audit.aMember')
      const from = memberRole(d.old_role)
      const to = memberRole(d.new_role)
      return from && to
        ? { key: 'audit.role.changed', values: { who, old: from, new: to }, item: null }
        : { key: 'audit.role.changed.unknown', values: { who }, item: null }
    }
    case 'member.removed': {
      const who = a.target_id ? ctx.person(a.target_id) : undefined
      return who ? { key: 'audit.member.removed', values: { who }, item: null } : null
    }
    case 'share_link.created':
    case 'share_link.revoked':
      if (a.target_type !== 'node') return null
      return { key: a.action === 'share_link.created' ? 'audit.link.created' : 'audit.link.revoked', values: {}, item }
    default:
      return null
  }
}
