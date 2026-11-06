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
