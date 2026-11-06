// The in-memory data of the mock API. Every value is typed by the contract (src/api/schema.d.ts).
// `resetDb()` makes a new copy, so each test starts from the same data.

import type { Schemas } from '../api/client'

type Node = Schemas['Node']
type Version = Schemas['Version']

export const ids = {
  ada: '0190a000-0000-7000-8000-00000000a0a0',
  charles: '0190a000-0000-7000-8000-00000000c0c0',
  grace: '0190a000-0000-7000-8000-00000000e0e0',
  personal: '0190a000-0000-7000-8000-000000000001',
  team: '0190a000-0000-7000-8000-000000000002',
  personalRoot: '0190a000-0000-7000-8000-000000000101',
  teamRoot: '0190a000-0000-7000-8000-000000000102',
  documents: '0190a000-0000-7000-8000-000000000201',
  photos: '0190a000-0000-7000-8000-000000000202',
  notes: '0190a000-0000-7000-8000-000000000203',
  report: '0190a000-0000-7000-8000-000000000204',
  plan: '0190a000-0000-7000-8000-000000000205',
  oldDraft: '0190a000-0000-7000-8000-000000000206',
  teamSpecs: '0190a000-0000-7000-8000-000000000207',
  sharedDoc: '0190a000-0000-7000-8000-000000000208',
  agentClaude: '0190a000-0000-7000-8000-000000000301',
  agentBackup: '0190a000-0000-7000-8000-000000000302',
  tagImportant: '0190a000-0000-7000-8000-000000000401',
  tagDraft: '0190a000-0000-7000-8000-000000000402',
  session: '0190a000-0000-7000-8000-000000000501',
  oauthClient: 'claude-desktop',
  // A tenant where Ada is a guest: she sees only the folder "Engine drawings" (shared with her),
  // not the root or "Projects" above it.
  guest: '0190a000-0000-7000-8000-000000000003',
  guestRoot: '0190a000-0000-7000-8000-000000000103',
  guestProjects: '0190a000-0000-7000-8000-000000000209',
  guestDrawings: '0190a000-0000-7000-8000-00000000020a',
  guestSketches: '0190a000-0000-7000-8000-00000000020b',
  guestGears: '0190a000-0000-7000-8000-00000000020c',
}

const t0 = '2026-10-01T09:00:00Z'

export function userRef(id = ids.ada): Schemas['PrincipalRef'] {
  const names: Record<string, string> = {
    [ids.ada]: 'Ada Lovelace',
    [ids.charles]: 'Charles Babbage',
    [ids.grace]: 'Grace Hopper',
  }
  return { kind: 'user', id, display_name: names[id] ?? 'Unknown user' }
}

export function byUser(id = ids.ada): Schemas['ActorPair'] {
  return { actor: userRef(id), on_behalf_of: null }
}

function node(p: Partial<Node> & Pick<Node, 'id' | 'name' | 'kind' | 'tenant_id'>): Node {
  return {
    parent_id: null,
    type: null,
    link_kind: null,
    link_target: null,
    head_version: null,
    owner_id: ids.ada,
    size_bytes: 0,
    revision: 1,
    trashed_at: null,
    created_at: t0,
    updated_at: t0,
    created_by: byUser(),
    updated_by: byUser(),
    my_role: 'manage',
    my_can_share: true,
    editors_can_share: null,
    editors_can_share_effective: true,
    ...p,
  }
}

function version(nodeId: string, id: string, text: string, at: string, p: Partial<Version> = {}): Version {
  return {
    id,
    tenant_id: ids.personal,
    node_id: nodeId,
    size_bytes: new TextEncoder().encode(text).length,
    content_hash: `sha256:${id.slice(-12).padStart(64, '0')}`,
    content_type: 'text/plain',
    restored_from: null,
    label: null,
    actor: userRef(),
    on_behalf_of: null,
    created_at: at,
    ...p,
  }
}

export type MockDb = ReturnType<typeof seed>

function seed() {
  const user: Schemas['User'] = {
    id: ids.ada,
    home_tenant_id: ids.personal,
    email: 'ada@example.com',
    email_verified: true,
    display_name: 'Ada Lovelace',
    avatar_url: null,
    created_at: t0,
  }
  const policy: Schemas['TenantPolicy'] = {
    allow_share_links: true,
    allow_edit_links: false,
    share_link_max_days: 30,
    allow_guests: true,
    members_can_share: true,
  }
  const tenants: Schemas['TenantMembership'][] = [
    {
      role: 'owner',
      tenant: {
        id: ids.personal,
        name: 'Ada Lovelace',
        kind: 'personal',
        plan: 'free',
        region: 'hel1',
        root_node_id: ids.personalRoot,
        policy: { ...policy },
        created_at: t0,
      },
    },
    {
      role: 'admin',
      tenant: {
        id: ids.team,
        name: 'Analytical Engines',
        kind: 'team',
        plan: 'team',
        region: 'hel1',
        root_node_id: ids.teamRoot,
        policy: { ...policy, allow_edit_links: true },
        created_at: t0,
      },
    },
    {
      role: 'guest',
      tenant: {
        id: ids.guest,
        name: 'Babbage & Co',
        kind: 'team',
        plan: 'team',
        region: 'hel1',
        root_node_id: ids.guestRoot,
        policy: { ...policy },
        created_at: t0,
      },
    },
  ]

  const v = {
    notes1: '0190a000-0000-7000-8000-000000000601',
    notes2: '0190a000-0000-7000-8000-000000000602',
    notes3: '0190a000-0000-7000-8000-000000000603',
    report1: '0190a000-0000-7000-8000-000000000604',
    plan1: '0190a000-0000-7000-8000-000000000605',
    specs1: '0190a000-0000-7000-8000-000000000606',
    shared1: '0190a000-0000-7000-8000-000000000607',
    gears1: '0190a000-0000-7000-8000-000000000608',
  }
  const nodes: Node[] = [
    node({ id: ids.personalRoot, tenant_id: ids.personal, name: '', kind: 'folder' }),
    node({ id: ids.teamRoot, tenant_id: ids.team, name: '', kind: 'folder' }),
    node({ id: ids.documents, tenant_id: ids.personal, parent_id: ids.personalRoot, name: 'Documents', kind: 'folder' }),
    node({ id: ids.photos, tenant_id: ids.personal, parent_id: ids.personalRoot, name: 'Photos', kind: 'folder' }),
    node({
      id: ids.notes,
      tenant_id: ids.personal,
      parent_id: ids.personalRoot,
      name: 'notes.md',
      kind: 'file',
      head_version: v.notes3,
      size_bytes: 52,
      revision: 3,
      updated_at: '2026-11-05T16:20:00Z',
      updated_by: { actor: { kind: 'agent', id: ids.agentClaude, display_name: 'Claude Desktop' }, on_behalf_of: ids.ada },
    }),
    node({
      id: ids.report,
      tenant_id: ids.personal,
      parent_id: ids.documents,
      name: 'report.txt',
      kind: 'file',
      head_version: v.report1,
      size_bytes: 30,
    }),
    node({
      id: ids.plan,
      tenant_id: ids.personal,
      parent_id: ids.documents,
      name: 'plan.txt',
      kind: 'file',
      head_version: v.plan1,
      size_bytes: 18,
    }),
    node({
      id: ids.oldDraft,
      tenant_id: ids.personal,
      parent_id: ids.documents,
      name: 'old-draft.txt',
      kind: 'file',
      trashed_at: '2026-11-04T10:00:00Z',
    }),
    node({
      id: ids.teamSpecs,
      tenant_id: ids.team,
      parent_id: ids.teamRoot,
      name: 'engine-specs.txt',
      kind: 'file',
      head_version: v.specs1,
      size_bytes: 21,
      owner_id: ids.charles,
      created_by: byUser(ids.charles),
      updated_by: byUser(ids.charles),
    }),
    node({
      id: ids.sharedDoc,
      tenant_id: ids.personal,
      parent_id: null,
      name: 'Difference engine.txt',
      kind: 'file',
      head_version: v.shared1,
      size_bytes: 24,
      owner_id: ids.charles,
      my_role: 'comment',
      my_can_share: false,
      created_by: byUser(ids.charles),
      updated_by: byUser(ids.charles),
    }),
    ...[
      { id: ids.guestRoot, parent_id: null, name: '', kind: 'folder' as const },
      { id: ids.guestProjects, parent_id: ids.guestRoot, name: 'Projects', kind: 'folder' as const },
      { id: ids.guestDrawings, parent_id: ids.guestProjects, name: 'Engine drawings', kind: 'folder' as const, my_role: 'edit' as const },
      { id: ids.guestSketches, parent_id: ids.guestDrawings, name: 'Sketches', kind: 'folder' as const, my_role: 'edit' as const },
      { id: ids.guestGears, parent_id: ids.guestDrawings, name: 'gears.txt', kind: 'file' as const, my_role: 'edit' as const, head_version: v.gears1, size_bytes: 14 },
    ].map((p) =>
      node({
        my_role: 'manage',
        ...p,
        tenant_id: ids.guest,
        owner_id: ids.charles,
        my_can_share: false,
        created_by: byUser(ids.charles),
        updated_by: byUser(ids.charles),
      }),
    ),
  ]
  /** Nodes that Ada cannot view: `getNode` answers 404, `getRoot` 403. */
  const hidden = new Set<string>([ids.guestRoot, ids.guestProjects])

  const versions: Version[] = [
    version(ids.notes, v.notes1, '# Notes\n\nFirst idea.\n', '2026-10-02T10:00:00Z', { label: 'First draft' }),
    version(ids.notes, v.notes2, '# Notes\n\nFirst idea.\nSecond idea.\n', '2026-10-20T11:00:00Z'),
    version(ids.notes, v.notes3, '# Notes\n\nFirst idea, refined.\nSecond idea.\nThird idea.\n', '2026-11-05T16:20:00Z', {
      actor: { kind: 'agent', id: ids.agentClaude, display_name: 'Claude Desktop' },
      on_behalf_of: ids.ada,
    }),
    version(ids.report, v.report1, 'Quarterly report: all is well.', t0),
    version(ids.plan, v.plan1, 'Plan: build Ruum.\n', t0),
    version(ids.teamSpecs, v.specs1, 'Mill and store specs.', t0, { tenant_id: ids.team }),
    version(ids.sharedDoc, v.shared1, 'Tables of polynomials.\n', t0),
    version(ids.guestGears, v.gears1, 'Gear ratios.\n', t0, { tenant_id: ids.guest }),
  ]
  const contents = new Map<string, string>([
    [v.notes1, '# Notes\n\nFirst idea.\n'],
    [v.notes2, '# Notes\n\nFirst idea.\nSecond idea.\n'],
    [v.notes3, '# Notes\n\nFirst idea, refined.\nSecond idea.\nThird idea.\n'],
    [v.report1, 'Quarterly report: all is well.'],
    [v.plan1, 'Plan: build Ruum.\n'],
    [v.specs1, 'Mill and store specs.'],
    [v.shared1, 'Tables of polynomials.\n'],
    [v.gears1, 'Gear ratios.\n'],
  ])

  const tags: Schemas['Tag'][] = [
    { id: ids.tagImportant, tenant_id: ids.personal, name: 'important', color: '#d1242f', created_by: byUser(), created_at: t0 },
    { id: ids.tagDraft, tenant_id: ids.personal, name: 'draft', color: '#9a6700', created_by: byUser(), created_at: t0 },
  ]
  const nodeTags: { node_id: string; tag_id: string }[] = [{ node_id: ids.notes, tag_id: ids.tagImportant }]

  const grants: Schemas['Grant'][] = [
    {
      id: '0190a000-0000-7000-8000-000000000701',
      node_id: ids.notes,
      grantee: { type: 'user', id: ids.charles, display_name: 'Charles Babbage', email: 'charles@example.com' },
      role: 'edit',
      inherited_from: null,
      created_by: byUser(),
      created_at: t0,
    },
    {
      id: '0190a000-0000-7000-8000-000000000702',
      node_id: ids.report,
      grantee: { type: 'email', email: 'grace@example.com' },
      role: 'view',
      inherited_from: ids.documents,
      created_by: byUser(),
      created_at: t0,
    },
  ]
  const shareLinks: Schemas['ShareLink'][] = [
    {
      id: '0190a000-0000-7000-8000-000000000801',
      node_id: ids.report,
      role: 'view',
      expires_at: '2026-12-01T00:00:00Z',
      has_password: false,
      created_by: byUser(),
      created_at: t0,
      revoked_at: null,
    },
  ]
  const shared: Schemas['SharedItem'][] = [
    { node: nodes.find((n) => n.id === ids.sharedDoc)!, role: 'comment', shared_by: byUser(ids.charles), shared_at: '2026-10-15T08:00:00Z' },
    { node: nodes.find((n) => n.id === ids.guestDrawings)!, role: 'edit', shared_by: byUser(ids.charles), shared_at: '2026-10-20T08:00:00Z' },
  ]

  const agents: Schemas['Agent'][] = [
    {
      id: ids.agentClaude,
      tenant_id: ids.personal,
      owner_id: ids.ada,
      name: 'Claude Desktop',
      description: 'Writes and edits my notes.',
      scope: { subtree: null, max_role: 'edit', expires_at: null, budget: { max_storage_bytes: null, max_writes_per_hour: 500 } },
      disabled_at: null,
      disabled_reason: null,
      created_at: t0,
      last_used_at: '2026-11-05T16:20:00Z',
    },
    {
      id: ids.agentBackup,
      tenant_id: ids.personal,
      owner_id: ids.ada,
      name: 'Nightly backup',
      description: null,
      scope: { subtree: ids.documents, max_role: 'view', expires_at: '2027-01-01T00:00:00Z' },
      disabled_at: '2026-10-30T00:00:00Z',
      disabled_reason: 'by_owner',
      created_at: t0,
      last_used_at: null,
    },
  ]
  const agentKeys: Schemas['AgentKey'][] = [
    {
      id: '0190a000-0000-7000-8000-000000000901',
      agent_id: ids.agentClaude,
      name: 'laptop',
      prefix: 'ruum_agent_7f3a',
      created_at: t0,
      expires_at: '2027-10-01T00:00:00Z',
      last_used_at: '2026-11-05T16:20:00Z',
      revoked_at: null,
    },
  ]
  const claudeRef: Schemas['PrincipalRef'] = { kind: 'agent', id: ids.agentClaude, display_name: 'Claude Desktop' }
  const journal: Schemas['JournalEntry'][] = [
    { tenant_id: ids.personal, seq: 41, node_id: ids.notes, revision: 2, type: 'version.created', actor: claudeRef, on_behalf_of: ids.ada, at: '2026-11-05T16:10:00Z', data: { name: 'notes.md' } },
    { tenant_id: ids.personal, seq: 42, node_id: ids.notes, revision: 3, type: 'version.created', actor: claudeRef, on_behalf_of: ids.ada, at: '2026-11-05T16:20:00Z', data: { name: 'notes.md' } },
    { tenant_id: ids.personal, seq: 43, node_id: ids.plan, revision: 1, type: 'node.renamed', actor: claudeRef, on_behalf_of: ids.ada, at: '2026-11-05T16:25:00Z', data: { from: 'todo.txt', to: 'plan.txt' } },
  ]

  const members: Schemas['Member'][] = [
    { tenant_id: ids.team, user_id: ids.charles, email: 'charles@example.com', display_name: 'Charles Babbage', role: 'owner', joined_at: t0 },
    { tenant_id: ids.team, user_id: ids.ada, email: 'ada@example.com', display_name: 'Ada Lovelace', role: 'admin', joined_at: t0 },
    { tenant_id: ids.team, user_id: ids.grace, email: 'grace@example.com', display_name: 'Grace Hopper', role: 'member', joined_at: '2026-10-10T12:00:00Z' },
    { tenant_id: ids.personal, user_id: ids.ada, email: 'ada@example.com', display_name: 'Ada Lovelace', role: 'owner', joined_at: t0 },
  ]
  const invitations: Schemas['Invitation'][] = [
    {
      id: '0190a000-0000-7000-8000-000000000a01',
      tenant_id: ids.team,
      email: 'alan@example.com',
      role: 'member',
      invited_by: byUser(),
      created_at: '2026-11-01T10:00:00Z',
      expires_at: '2026-11-15T10:00:00Z',
      accepted_at: null,
    },
  ]
  const audit: Schemas['AuditEntry'][] = [
    { id: '0190a000-0000-7000-8000-000000000b01', action: 'member.invited', actor: userRef(), on_behalf_of: null, target_type: 'invitation', target_id: 'alan@example.com', ip: '127.0.0.1', at: '2026-11-01T10:00:00Z', data: {} },
    { id: '0190a000-0000-7000-8000-000000000b02', action: 'agent.registered', actor: userRef(), on_behalf_of: null, target_type: 'agent', target_id: ids.agentClaude, ip: '127.0.0.1', at: t0, data: {} },
    { id: '0190a000-0000-7000-8000-000000000b03', action: 'tenant.policy_updated', actor: userRef(ids.charles), on_behalf_of: null, target_type: 'tenant', target_id: ids.team, ip: '127.0.0.1', at: '2026-10-05T09:00:00Z', data: { allow_edit_links: true } },
  ]
  const limits: Record<string, Schemas['LimitsAndUsage']> = {
    [ids.personal]: {
      plan: 'free',
      limits: { storage_bytes: 5 * 2 ** 30, max_file_bytes: 2 ** 30, version_days: 30, version_count: 100, trash_days: 30, admin_restore_days: 0, agents: 3, members: 1 },
      usage: { storage_bytes: 1_234_567_890, nodes: 8, agents: 2, members: 1 },
    },
    [ids.team]: {
      plan: 'team',
      limits: { storage_bytes: 1024 * 2 ** 30, max_file_bytes: 10 * 2 ** 30, version_days: 365, version_count: null, trash_days: 30, admin_restore_days: 90, agents: 50, members: null },
      usage: { storage_bytes: 98_765_432_100, nodes: 1520, agents: 7, members: 3 },
    },
  }

  const identities: Schemas['Identity'][] = [
    { id: '0190a000-0000-7000-8000-000000000c01', issuer: 'https://accounts.google.com', subject: '1094', provider: 'google', email: 'ada@example.com', email_verified: true, linked_at: t0 },
    { id: '0190a000-0000-7000-8000-000000000c02', issuer: 'https://auth.ruum.localhost', subject: 'pk-1', provider: 'passkey', email: null, email_verified: false, linked_at: '2026-10-12T08:00:00Z' },
  ]
  const sessions: Schemas['Session'][] = [
    { id: ids.session, created_at: '2026-11-05T08:00:00Z', expires_at: '2026-11-19T08:00:00Z', last_seen_at: '2026-11-06T00:50:00Z', user_agent: 'Firefox on macOS', current: true },
    { id: '0190a000-0000-7000-8000-000000000502', created_at: '2026-11-01T08:00:00Z', expires_at: '2026-11-15T08:00:00Z', last_seen_at: '2026-11-03T21:00:00Z', user_agent: 'Safari on iPhone', current: false },
  ]
  const appPasswords: Schemas['AppPassword'][] = [
    { id: '0190a000-0000-7000-8000-000000000d01', tenant_id: ids.personal, name: 'Finder (WebDAV)', prefix: 'ruum_app_19c2', created_at: t0, expires_at: null, last_used_at: '2026-11-02T12:00:00Z', revoked_at: null },
  ]
  const oauthClients: Schemas['OAuthClient'][] = [
    {
      client_id: ids.oauthClient,
      client_id_issued_at: 1790000000,
      client_name: 'Claude Desktop',
      redirect_uris: ['https://claude.example/callback'],
      grant_types: ['authorization_code', 'refresh_token'],
      response_types: ['code'],
      token_endpoint_auth_method: 'none',
      client_uri: 'https://claude.example',
      logo_uri: null,
    },
  ]

  return {
    signedIn: true,
    seq: 100,
    user,
    tenants,
    nodes,
    hidden,
    versions,
    contents,
    tags,
    nodeTags,
    grants,
    shareLinks,
    shared,
    agents,
    agentKeys,
    journal,
    members,
    invitations,
    audit,
    limits,
    identities,
    sessions,
    appPasswords,
    oauthClients,
  }
}

export let db: MockDb = seed()

/** Makes a new copy of the seed data. */
export function resetDb(): MockDb {
  db = seed()
  return db
}
