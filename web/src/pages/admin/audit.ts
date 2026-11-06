import type { Schemas } from '../../api/client'

/** Readable names of the activity log's actions (ia.md: "Activity log"; never the dotted name). */
const actions: Record<string, string> = {
  'auth.login': 'signed in',
  'auth.login_failed': 'could not sign in',
  'auth.logout': 'signed out',
  'auth.app_password_created': 'made an app password',
  'auth.app_password_revoked': 'turned off an app password',
  'auth.app_password_used': 'used an app password',
  'app_password.created': 'made an app password',
  'app_password.revoked': 'turned off an app password',
  'agent.registered': 'created an agent',
  'agent.updated': "changed an agent's access",
  'agent.disabled': 'paused an agent',
  'agent.enabled': 'resumed an agent',
  'agent.key_created': 'made an agent key',
  'agent.key_revoked': 'turned off an agent key',
  'agent.key_used': 'used an agent key',
  'agent.key_refused': 'used a key that does not work',
  'agent.oauth_approved': 'connected an app',
  'agent.oauth_denied': "didn't allow an app to connect",
  'agent.token_issued': 'started working',
  'agent.undo': "undid an agent's changes",
  'email_grants.bound': 'joined and got the items shared by email',
  'grant.added': 'gave access',
  'grant.updated': 'changed access',
  'grant.removed': 'removed access',
  'invitation.created': 'invited someone',
  'invitation.accepted': 'accepted an invitation',
  'invitation.revoked': 'cancelled an invitation',
  'member.added': 'added a member',
  'member.updated': 'changed a member',
  'member.role_changed': "changed a member's role",
  'member.removed': 'removed a member',
  'node.created': 'created',
  'node.copied': 'copied',
  'node.moved': 'moved',
  'node.renamed': 'renamed',
  'node.trashed': 'moved to the trash',
  'node.restored': 'restored from the trash',
  'node.purged': 'deleted for good',
  'node.exploded': 'unpacked',
  'node.sharing_updated': 'changed the sharing rule of',
  'share_link.created': 'made a link to',
  'share_link.revoked': 'turned off a link to',
  'tag.attached': 'added a tag to',
  'tag.detached': 'removed a tag from',
  'tenant.created': 'created the space',
  'tenant.updated': 'changed the space settings',
  'version.created': 'saved a new version of',
  'version.labeled': 'named a version of',
  'version.restored': 'restored a version of',
}

const str = (v: unknown) => (typeof v === 'string' && v ? v : null)

/** "moved Plan.pdf", "invited ann@example.com": the action and its object in plain words. */
export function auditText(a: Schemas['AuditEntry']): string {
  const verb = actions[a.action] ?? a.action.replace(/[._]/g, ' ')
  const d = a.data ?? {}
  const object =
    str(d.name) ?? str(d.new_name) ?? str(d.node_name) ?? str(d.email) ?? str(d.agent_name) ?? str(d.tag_name) ?? str(d.label) ?? null
  const joins = / (of|to|from)$/.test(verb) || /^(created|copied|moved|renamed|unpacked)$/.test(verb)
  if (object) return joins ? `${verb} ${object}` : `${verb}: ${object}`
  if (a.target_type === 'node') return joins ? `${verb} an item` : `${verb}: an item`
  return verb
}

/** What the activity log knows about the items and people an entry names. */
export type AuditContext = {
  /** The name of a user, or `undefined` when unknown. */
  person: (id: string) => string | undefined
  /** The name of the space (for a grant to everyone in it). */
  spaceName: string
}

/** One part of a detailed entry: plain text, or an item the row links to. */
export type AuditPart = string | { item: string }

const roleWords: Record<string, string> = { view: 'view', comment: 'comment', edit: 'edit', manage: 'manage' }
const memberRoleWords: Record<string, string> = { owner: 'Owner', admin: 'Admin', member: 'Member', guest: 'Guest' }
const ruleWords = (v: unknown) => (v === true ? 'yes' : v === false ? 'no' : 'same as the folder above')

function granteeText(g: unknown, ctx: AuditContext): string | null {
  if (!g || typeof g !== 'object') return null
  const x = g as Record<string, unknown>
  switch (x.type) {
    case 'user':
      return (typeof x.id === 'string' ? ctx.person(x.id) : undefined) ?? str(x.display_name) ?? str(x.email) ?? 'a person'
    case 'email':
      return str(x.email)
    case 'tenant_members':
      return `everyone in ${ctx.spaceName}`
    default:
      return null
  }
}

/**
 * The detailed form of the entries an admin needs to answer "who gave whom access to what":
 * access, role and sharing rule changes name the item, the person and the old and new values.
 * `null` for other entries (they use `auditText`). The item is a separate part so the row can
 * link to it.
 */
export function auditDetail(a: Schemas['AuditEntry'], ctx: AuditContext): AuditPart[] | null {
  const d = (a.data ?? {}) as Record<string, unknown>
  const item: AuditPart = { item: a.target_id ?? '' }
  const role = (v: unknown) => (typeof v === 'string' ? (roleWords[v] ?? v) : 'some')
  switch (a.action) {
    case 'grant.added': {
      const who = granteeText(d.grantee, ctx)
      if (!who || a.target_type !== 'node') return null
      return [`gave ${who} ${role(d.role)} access to `, item]
    }
    case 'grant.updated': {
      const who = granteeText(d.grantee, ctx)
      if (!who || a.target_type !== 'node') return null
      return [`changed the access of ${who} to `, item, ` from ${role(d.old_role)} to ${role(d.new_role)}`]
    }
    case 'grant.removed': {
      const who = granteeText(d.grantee, ctx)
      if (!who || a.target_type !== 'node') return null
      return [`removed the ${role(d.role)} access of ${who} to `, item]
    }
    case 'node.sharing_updated':
      if (a.target_type !== 'node') return null
      return [
        'changed the sharing rule of ',
        item,
        `: people who can edit can also share, from ${ruleWords(d.old_editors_can_share)} to ${ruleWords(d.new_editors_can_share)}`,
      ]
    case 'member.role_changed': {
      const who = (a.target_id && ctx.person(a.target_id)) ?? 'a member'
      const from = typeof d.old_role === 'string' ? (memberRoleWords[d.old_role] ?? d.old_role) : null
      const to = typeof d.new_role === 'string' ? (memberRoleWords[d.new_role] ?? d.new_role) : null
      return from && to ? [`changed ${who} from ${from} to ${to}`] : [`changed the role of ${who}`]
    }
    case 'member.removed': {
      const who = a.target_id ? ctx.person(a.target_id) : undefined
      return who ? [`removed ${who} from the space`] : null
    }
    case 'share_link.created':
    case 'share_link.revoked':
      if (a.target_type !== 'node') return null
      return [a.action === 'share_link.created' ? 'made a link to ' : 'turned off a link to ', item]
    default:
      return null
  }
}
