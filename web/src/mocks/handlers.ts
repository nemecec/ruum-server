// Hand-written MSW handlers for the operations the web UI uses. Paths, bodies and answers are typed
// by the contract (src/api/schema.d.ts), so a contract change breaks the build here first.

import { http, HttpResponse, type HttpHandler } from 'msw'
import type { paths, Schemas } from '../api/client'
import { byUser, db, ids, userRef } from './db'

type Method = 'get' | 'post' | 'put' | 'patch' | 'delete'
type Op<P extends keyof paths, M extends Method> = NonNullable<paths[P][M]>
type Responses<P extends keyof paths, M extends Method> = Op<P, M> extends { responses: infer R } ? R : never
type Json<X> = X extends { content: { 'application/json': infer B } } ? B : never
type Ok<P extends keyof paths, M extends Method> = Json<Responses<P, M>[Extract<keyof Responses<P, M>, 200 | 201>]>
type Body<P extends keyof paths, M extends Method> = Op<P, M> extends {
  requestBody?: { content: { 'application/json': infer B } }
}
  ? B
  : undefined
type PathParams<P extends keyof paths, M extends Method> = Op<P, M> extends { parameters: { path: infer X } }
  ? X extends object
    ? { [K in keyof X]: string }
    : Record<string, string>
  : Record<string, string>

type Ctx<P extends keyof paths, M extends Method> = {
  params: PathParams<P, M>
  query: URLSearchParams
  body: Body<P, M>
  tenant: string
  request: Request
}

/** An error answer with the standard error body. Throw it from a resolver. */
class Fail {
  readonly status: number
  readonly code: Schemas['ErrorCode']
  readonly message: string
  constructor(status: number, code: Schemas['ErrorCode'], message: string) {
    this.status = status
    this.code = code
    this.message = message
  }
}

const notFound = (what: string) => new Fail(404, 'not_found', `${what} not found.`)

type Options = { status?: number; public?: boolean; rawBody?: boolean }

/** Latency of the mock API in the browser, so loading states are visible. Off in tests. */
let latencyMs = 0
export function setMockLatency(ms: number): void {
  latencyMs = ms
}

/** One request as the contract check sees it. */
export type RequestCheck = (req: {
  method: Method
  path: string
  headers: Headers
  body: unknown
  /** False when the body is raw bytes (`putContent`), not JSON. */
  jsonBody: boolean
}) => string[]

let requestCheck: RequestCheck | null = null
/** The contract problems of the requests so far. The tests fail when it is not empty. */
export const contractViolations: string[] = []

/**
 * Checks every request with `check` before the resolver runs. The tests set it to the check against
 * contracts/openapi.yaml (src/test/contract.ts), so a request that the real server refuses fails.
 */
export function setRequestCheck(check: RequestCheck | null): void {
  requestCheck = check
}

function cookie(name: string): string | undefined {
  if (typeof document === 'undefined') return undefined
  return document.cookie
    .split('; ')
    .find((c) => c.startsWith(`${name}=`))
    ?.slice(name.length + 1)
}

function route<P extends keyof paths, M extends Method>(
  method: M,
  path: P,
  resolve: (ctx: Ctx<P, M>) => Ok<P, M> | void | Promise<Ok<P, M> | void>,
  options: Options = {},
): HttpHandler {
  const mswPath = String(path).replace(/\{(\w+)\}/g, ':$1')
  return http[method](mswPath, async ({ request, params }) => {
    if (latencyMs) await new Promise((r) => setTimeout(r, latencyMs))
    try {
      if (!options.public && !db.signedIn) throw new Fail(401, 'unauthenticated', 'Sign in first.')
      let body: unknown = undefined
      if (!options.rawBody && method !== 'get' && method !== 'delete') {
        const text = await request.clone().text()
        body = text ? JSON.parse(text) : undefined
      }
      // Cookie sessions: a state-changing request must send `Ruum-Csrf` with the value of the cookie
      // `ruum_csrf` (double submit).
      if (method !== 'get' && !options.public) {
        const csrf = cookie('ruum_csrf')
        if (!csrf || request.headers.get('Ruum-Csrf') !== csrf) {
          contractViolations.push(`${method.toUpperCase()} ${String(path)}: Ruum-Csrf is missing or wrong.`)
          throw new Fail(403, 'permission_denied', 'The Ruum-Csrf header is missing or wrong.')
        }
      }
      const problems = requestCheck?.({ method, path: String(path), headers: request.headers, body, jsonBody: !options.rawBody }) ?? []
      if (problems.length) {
        contractViolations.push(...problems.map((p) => `${method.toUpperCase()} ${String(path)}: ${p}`))
        throw new Fail(422, 'validation_failed', problems.join(' '))
      }
      const result = await resolve({
        params: params as PathParams<P, M>,
        query: new URL(request.url).searchParams,
        body: body as Body<P, M>,
        tenant: request.headers.get('Ruum-Tenant') ?? '',
        request,
      })
      if (result === undefined) return new HttpResponse(null, { status: 204 })
      return HttpResponse.json(result as never, { status: options.status ?? 200 })
    } catch (e) {
      if (e instanceof Fail) {
        return HttpResponse.json({ error: { code: e.code, message: e.message } }, { status: e.status })
      }
      throw e
    }
  })
}

const now = () => new Date().toISOString()

/** Random characters of an API key secret (`[A-Za-z0-9_-]`). */
function secretChars(n: number): string {
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-'
  return Array.from(crypto.getRandomValues(new Uint8Array(n)), (b) => alphabet[b % 64]).join('')
}
const newId = () => crypto.randomUUID()

function change(): Schemas['Change'] {
  db.seq += 1
  return { seq: db.seq, actor: userRef(), on_behalf_of: null, at: now() }
}

function record(type: Schemas['EventType'], nodeId: string | null, tenant: string, data: Record<string, unknown> = {}) {
  const c = change()
  db.journal.push({ tenant_id: tenant, seq: c.seq!, node_id: nodeId, revision: null, type, actor: c.actor, on_behalf_of: null, at: c.at, data })
  return c
}

function findNode(id: string, tenant: string): Schemas['Node'] {
  const n = db.nodes.find((x) => x.id === id && x.tenant_id === tenant && !db.hidden.has(x.id))
  if (!n) throw notFound('Node')
  return n
}

function membership(tenant: string) {
  const m = db.tenants.find((t) => t.tenant.id === tenant)
  if (!m) throw new Fail(404, 'not_found', 'Tenant not found.')
  return m
}

function requireAdmin(tenant: string) {
  const m = membership(tenant)
  if (m.role !== 'admin' && m.role !== 'owner') throw new Fail(403, 'permission_denied', 'Only tenant admins can do this.')
  return m
}

function requireTenantHeader(tenant: string) {
  if (!tenant) throw new Fail(400, 'bad_request', 'The Ruum-Tenant header is missing.')
  membership(tenant)
}

/** One page of `items`, by the `limit` and `cursor` of the query. The cursor of the mock is an offset. */
function page<T>(items: T[], query?: URLSearchParams): { items: T[]; next_cursor: string | null } {
  const limit = Math.min(Math.max(Number(query?.get('limit') ?? 100) || 100, 1), 1000)
  const start = Number(query?.get('cursor') ?? 0) || 0
  const end = start + limit
  return { items: items.slice(start, end), next_cursor: end < items.length ? String(end) : null }
}

function touch(n: Schemas['Node']) {
  n.revision += 1
  n.updated_at = now()
  n.updated_by = byUser()
}

function nameTaken(parentId: string | null, name: string, tenant: string, except?: string) {
  return db.nodes.some(
    (n) => n.tenant_id === tenant && n.parent_id === parentId && n.name === name && !n.trashed_at && n.id !== except,
  )
}

function placeName(parentId: string, name: string, tenant: string, onConflict: string | undefined, except?: string) {
  if (!nameTaken(parentId, name, tenant, except)) return name
  if (onConflict !== 'rename') throw new Fail(409, 'name_taken', `A node named "${name}" already exists here.`)
  const dot = name.lastIndexOf('.')
  const [base, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, '']
  for (let i = 2; ; i++) {
    const candidate = `${base} (${i})${ext}`
    if (!nameTaken(parentId, candidate, tenant, except)) return candidate
  }
}

function isInside(nodeId: string, folderId: string, tenant: string): boolean {
  let cur: Schemas['Node'] | undefined = db.nodes.find((n) => n.id === nodeId && n.tenant_id === tenant)
  while (cur) {
    if (cur.id === folderId) return true
    const parent: string | null = cur.parent_id
    cur = parent ? db.nodes.find((n) => n.id === parent && n.tenant_id === tenant) : undefined
  }
  return false
}

function pathOf(n: Schemas['Node'], tenant: string): string {
  const parts: string[] = []
  let cur: Schemas['Node'] | undefined = n
  while (cur && cur.parent_id) {
    parts.unshift(cur.name)
    const parent: string = cur.parent_id
    cur = db.nodes.find((x) => x.id === parent && x.tenant_id === tenant)
  }
  return '/' + parts.join('/')
}

function base64(text: string): string {
  const bytes = new TextEncoder().encode(text)
  let bin = ''
  bytes.forEach((b) => (bin += String.fromCharCode(b)))
  return btoa(bin)
}

/** A naive line diff, enough for the versions panel of the mock. */
function lineDiff(a: string, b: string): Schemas['DiffResult'] {
  const left = a.split('\n')
  const right = b.split('\n')
  const changes: Schemas['DiffChange'][] = []
  const max = Math.max(left.length, right.length)
  const summary = { inserted: 0, deleted: 0, modified: 0, moved: 0 }
  for (let i = 0; i < max; i++) {
    const o = left[i]
    const n = right[i]
    if (o === n) continue
    if (o === undefined) {
      changes.push({ kind: 'insert', path: `line ${i + 1}`, new: n })
      summary.inserted++
    } else if (n === undefined) {
      changes.push({ kind: 'delete', path: `line ${i + 1}`, old: o })
      summary.deleted++
    } else {
      changes.push({ kind: 'modify', path: `line ${i + 1}`, old: o, new: n })
      summary.modified++
    }
  }
  return { plugin: 'text-lines', summary, changes, truncated: false }
}

export const handlers: HttpHandler[] = [
  // ------------------------------------------------------------------ auth and account
  route('post', '/v1/auth/logout', () => {
    db.signedIn = false
    if (typeof document !== 'undefined') document.cookie = 'ruum_mock_session=; Max-Age=0; Path=/'
    return { logout_url: '/signin?signed_out=1' }
  }),
  route('get', '/v1/me', () => ({ actor: userRef(), on_behalf_of: null, user: db.user, agent: null, bound_tenant_id: null })),
  route('get', '/v1/me/identities', () => ({ items: db.identities })),
  route('get', '/v1/me/sessions', () => ({ items: db.sessions })),
  route('delete', '/v1/me/sessions/{session_id}', ({ params }) => {
    const before = db.sessions.length
    db.sessions = db.sessions.filter((s) => s.id !== params.session_id)
    if (db.sessions.length === before) throw notFound('Session')
  }),
  route('get', '/v1/me/app-passwords', ({ tenant }) => ({
    items: db.appPasswords.filter((p) => p.tenant_id === tenant && !p.revoked_at),
  })),
  route(
    'post',
    '/v1/me/app-passwords',
    ({ body, tenant }) => {
      if (!body.name.trim()) throw new Fail(422, 'validation_failed', 'Give the app password a name.')
      const app_password: Schemas['AppPassword'] = {
        id: newId(),
        tenant_id: tenant,
        name: body.name,
        prefix: `ruum_app_${secretChars(4)}`,
        created_at: now(),
        expires_at: body.expires_at ?? null,
        last_used_at: null,
        revoked_at: null,
      }
      db.appPasswords.push(app_password)
      return { app_password, secret: `${app_password.prefix}${secretChars(39)}` }
    },
    { status: 201 },
  ),
  route('delete', '/v1/me/app-passwords/{app_password_id}', ({ params }) => {
    const p = db.appPasswords.find((x) => x.id === params.app_password_id)
    if (!p) throw notFound('App password')
    p.revoked_at = now()
  }),

  // ------------------------------------------------------------------ tenants
  route('get', '/v1/tenants', () => ({ items: db.tenants })),
  route(
    'post',
    '/v1/tenants',
    ({ body }) => {
      if (!body.name.trim()) throw new Fail(422, 'validation_failed', 'Give the team a name.')
      const id = newId()
      const root = newId()
      const tenant: Schemas['Tenant'] = {
        id,
        name: body.name,
        kind: 'team',
        plan: 'free',
        region: 'hel1',
        root_node_id: root,
        policy: { allow_share_links: true, allow_edit_links: false, share_link_max_days: null, allow_guests: true, members_can_share: true },
        created_at: now(),
      }
      db.tenants.push({ tenant, role: 'owner' })
      db.nodes.push({ ...db.nodes.find((n) => n.id === ids.personalRoot)!, id: root, tenant_id: id })
      db.members.push({ tenant_id: id, user_id: ids.ada, email: db.user.email, display_name: db.user.display_name, role: 'owner', joined_at: now() })
      db.limits[id] = { ...db.limits[ids.personal], usage: { storage_bytes: 0, nodes: 1, agents: 0, members: 1 } }
      return tenant
    },
    { status: 201 },
  ),
  route('get', '/v1/tenants/{tenant_id}', ({ params }) => membership(params.tenant_id).tenant),
  route('patch', '/v1/tenants/{tenant_id}', ({ params, body }) => {
    const m = requireAdmin(params.tenant_id)
    if (body.name !== undefined) m.tenant.name = body.name
    if (body.policy) m.tenant.policy = body.policy
    db.audit.unshift({ id: newId(), action: 'tenant.updated', actor: userRef(), on_behalf_of: null, target_type: 'tenant', target_id: m.tenant.id, at: now(), data: { ...body } })
    return m.tenant
  }),
  route('get', '/v1/tenants/{tenant_id}/members', ({ params, query }) => {
    membership(params.tenant_id)
    return page(db.members.filter((m) => m.tenant_id === params.tenant_id), query)
  }),
  route('patch', '/v1/tenants/{tenant_id}/members/{user_id}', ({ params, body }) => {
    requireAdmin(params.tenant_id)
    const m = db.members.find((x) => x.tenant_id === params.tenant_id && x.user_id === params.user_id)
    if (!m) throw notFound('Member')
    if (m.role === 'owner' && body.role !== 'owner') {
      const owners = db.members.filter((x) => x.tenant_id === params.tenant_id && x.role === 'owner')
      if (owners.length === 1) throw new Fail(409, 'conflict', 'A tenant needs at least one owner.')
    }
    m.role = body.role
    return m
  }),
  route('delete', '/v1/tenants/{tenant_id}/members/{user_id}', ({ params }) => {
    requireAdmin(params.tenant_id)
    const m = db.members.find((x) => x.tenant_id === params.tenant_id && x.user_id === params.user_id)
    if (!m) throw notFound('Member')
    if (m.role === 'owner') throw new Fail(409, 'conflict', 'Make another member the owner first.')
    db.members = db.members.filter((x) => x !== m)
  }),
  route('get', '/v1/tenants/{tenant_id}/invitations', ({ params, query }) => {
    requireAdmin(params.tenant_id)
    return page(db.invitations.filter((i) => i.tenant_id === params.tenant_id && !i.accepted_at), query)
  }),
  route(
    'post',
    '/v1/tenants/{tenant_id}/invitations',
    ({ params, body }) => {
      requireAdmin(params.tenant_id)
      if (!/^[^@\s]+@[^@\s]+$/.test(body.email)) throw new Fail(422, 'validation_failed', 'Enter a valid email address.')
      const inv: Schemas['Invitation'] = {
        id: newId(),
        tenant_id: params.tenant_id,
        email: body.email,
        role: body.role,
        invited_by: byUser(),
        created_at: now(),
        expires_at: new Date(Date.now() + 14 * 86400_000).toISOString(),
        accepted_at: null,
      }
      db.invitations.push(inv)
      return inv
    },
    { status: 201 },
  ),
  route('delete', '/v1/tenants/{tenant_id}/invitations/{invitation_id}', ({ params }) => {
    requireAdmin(params.tenant_id)
    db.invitations = db.invitations.filter((i) => i.id !== params.invitation_id)
  }),
  route('get', '/v1/tenants/{tenant_id}/limits', ({ params }) => {
    membership(params.tenant_id)
    const l = db.limits[params.tenant_id]
    if (!l) throw notFound('Limits')
    return l
  }),
  route('get', '/v1/tenants/{tenant_id}/audit', ({ params, query }) => {
    requireAdmin(params.tenant_id)
    const action = query.get('action')
    return page(db.audit.filter((a) => !action || a.action.startsWith(action)), query)
  }),

  // ------------------------------------------------------------------ agents
  route('get', '/v1/agents', ({ tenant, query }) => {
    requireTenantHeader(tenant)
    return page(db.agents.filter((a) => a.tenant_id === tenant), query)
  }),
  route(
    'post',
    '/v1/agents',
    ({ body, tenant }) => {
      requireTenantHeader(tenant)
      if (!body.name.trim()) throw new Fail(422, 'validation_failed', 'Give the agent a name.')
      const limit = db.limits[tenant]?.limits.agents ?? 100
      if (db.agents.filter((a) => a.tenant_id === tenant).length >= limit) {
        throw new Fail(403, 'plan_limit_reached', `Your plan allows ${limit} agents.`)
      }
      const agent: Schemas['Agent'] = {
        id: newId(),
        tenant_id: tenant,
        owner_id: ids.ada,
        name: body.name,
        description: body.description ?? null,
        scope: body.scope,
        disabled_at: null,
        disabled_reason: null,
        created_at: now(),
        last_used_at: null,
      }
      db.agents.push(agent)
      return agent
    },
    { status: 201 },
  ),
  route('get', '/v1/agents/{agent_id}', ({ params, tenant }) => {
    const a = db.agents.find((x) => x.id === params.agent_id && x.tenant_id === tenant)
    if (!a) throw notFound('Agent')
    return a
  }),
  route('patch', '/v1/agents/{agent_id}', ({ params, body, tenant }) => {
    const a = db.agents.find((x) => x.id === params.agent_id && x.tenant_id === tenant)
    if (!a) throw notFound('Agent')
    if (body.name !== undefined) a.name = body.name
    if (body.description !== undefined) a.description = body.description
    if (body.scope) a.scope = body.scope
    return a
  }),
  route('post', '/v1/agents/{agent_id}/disable', ({ params, tenant }) => {
    const a = db.agents.find((x) => x.id === params.agent_id && x.tenant_id === tenant)
    if (!a) throw notFound('Agent')
    a.disabled_at = now()
    a.disabled_reason = 'by_owner'
    return a
  }),
  route('post', '/v1/agents/{agent_id}/enable', ({ params, tenant }) => {
    const a = db.agents.find((x) => x.id === params.agent_id && x.tenant_id === tenant)
    if (!a) throw notFound('Agent')
    a.disabled_at = null
    a.disabled_reason = null
    return a
  }),
  route('get', '/v1/agents/{agent_id}/keys', ({ params, query }) => page(db.agentKeys.filter((k) => k.agent_id === params.agent_id), query)),
  route(
    'post',
    '/v1/agents/{agent_id}/keys',
    ({ params, body }) => {
      const key: Schemas['AgentKey'] = {
        id: newId(),
        agent_id: params.agent_id,
        name: body?.name ?? null,
        prefix: `ruum_agent_${secretChars(4)}`,
        created_at: now(),
        expires_at: body?.expires_at ?? new Date(Date.now() + 365 * 86400_000).toISOString(),
        last_used_at: null,
        revoked_at: null,
      }
      db.agentKeys.push(key)
      return { key, secret: `${key.prefix}${secretChars(39)}` }
    },
    { status: 201 },
  ),
  route('delete', '/v1/agents/{agent_id}/keys/{key_id}', ({ params }) => {
    const k = db.agentKeys.find((x) => x.id === params.key_id && x.agent_id === params.agent_id)
    if (!k) throw notFound('Key')
    k.revoked_at = now()
  }),
  route('get', '/v1/agents/{agent_id}/activity', ({ params, tenant, query }) =>
    page(
      db.journal.filter((j) => j.tenant_id === tenant && j.actor.kind === 'agent' && j.actor.id === params.agent_id).reverse(),
      query,
    ),
  ),
  route('post', '/v1/agents/{agent_id}/undo', ({ params, body, tenant }) => {
    if (!body.since && !body.seqs) throw new Fail(422, 'validation_failed', 'Give since (and optionally until), or seqs.')
    const entries = db.journal.filter(
      (j) =>
        j.tenant_id === tenant &&
        j.actor.kind === 'agent' &&
        j.actor.id === params.agent_id &&
        (!body.seqs || body.seqs.includes(j.seq)) &&
        (!body.since || j.at >= body.since) &&
        (!body.until || j.at <= body.until),
    )
    const item = (j: Schemas['JournalEntry'], reason: Schemas['UndoItem']['reason']): Schemas['UndoItem'] => ({
      seq: j.seq,
      node_id: j.node_id,
      type: j.type,
      reason,
    })
    // The mock treats a purge as irreversible and everything else as revertible.
    const reverted = entries.filter((j) => j.type !== 'node.purged').map((j) => item(j, null))
    const skipped = entries.filter((j) => j.type === 'node.purged').map((j) => item(j, 'irreversible'))
    if (body.dry_run) return { dry_run: true, reverted, skipped, change: null }
    db.journal = db.journal.filter((j) => !reverted.some((r) => r.seq === j.seq))
    return { dry_run: false, reverted, skipped, change: change() }
  }),
  route('get', '/v1/oauth/clients/{client_id}', ({ params }) => {
    const c = db.oauthClients.find((x) => x.client_id === params.client_id)
    if (!c) throw notFound('OAuth client')
    return c
  }),
  route('post', '/v1/oauth/consent', ({ body }) => {
    const redirect = 'https://claude.example/callback'
    if (!body.request) throw new Fail(422, 'validation_failed', 'The authorization request is missing.')
    if (!body.approve) return { redirect_to: `${redirect}?error=access_denied` }
    if (!body.tenant_id) throw new Fail(422, 'validation_failed', 'Pick a tenant.')
    if (body.new_agent) {
      db.agents.push({
        id: newId(),
        tenant_id: body.tenant_id,
        owner_id: ids.ada,
        name: body.new_agent.name,
        description: body.new_agent.description ?? null,
        scope: body.new_agent.scope,
        disabled_at: null,
        disabled_reason: null,
        created_at: now(),
        last_used_at: null,
      })
    } else if (!body.agent_id) {
      throw new Fail(422, 'validation_failed', 'Pick an agent or create a new one.')
    }
    return { redirect_to: `${redirect}?code=mock-code` }
  }),

  // ------------------------------------------------------------------ nodes
  route('get', '/v1/nodes/root', ({ tenant }) => {
    requireTenantHeader(tenant)
    const rootId = membership(tenant).tenant.root_node_id
    if (db.hidden.has(rootId)) throw new Fail(403, 'permission_denied', 'You cannot view the root of this tenant.')
    return findNode(rootId, tenant)
  }),
  route(
    'post',
    '/v1/nodes',
    ({ body, tenant }) => {
      const parent = findNode(body.parent_id, tenant)
      if (parent.kind !== 'folder') throw new Fail(422, 'validation_failed', 'The parent is not a folder.')
      if (!body.name.trim() || /[/\\]/.test(body.name)) throw new Fail(422, 'validation_failed', 'Names cannot be empty or contain slashes.')
      const name = placeName(parent.id, body.name, tenant, body.on_conflict)
      const n: Schemas['Node'] = {
        ...parent,
        id: newId(),
        parent_id: parent.id,
        name,
        kind: body.kind,
        type: body.type ?? null,
        head_version: null,
        size_bytes: 0,
        revision: 1,
        created_at: now(),
        updated_at: now(),
        created_by: byUser(),
        updated_by: byUser(),
        my_role: 'manage',
        my_can_share: true,
        editors_can_share: null,
      }
      db.nodes.push(n)
      return { node: n, change: record('node.created', n.id, tenant, { name }) }
    },
    { status: 201 },
  ),
  route('get', '/v1/nodes/{node_id}', ({ params, tenant }) => findNode(params.node_id, tenant)),
  route('delete', '/v1/nodes/{node_id}', ({ params, tenant }) => {
    const n = findNode(params.node_id, tenant)
    if (!n.trashed_at) throw new Fail(409, 'conflict', 'Move the node to the trash first.')
    db.nodes = db.nodes.filter((x) => !isInside(x.id, n.id, tenant))
    return { change: record('node.purged', n.id, tenant) }
  }),
  route('get', '/v1/nodes/{node_id}/children', ({ params, tenant, query }) => {
    const parent = findNode(params.node_id, tenant)
    const items = db.nodes
      .filter((n) => n.tenant_id === tenant && n.parent_id === parent.id && !n.trashed_at && !db.hidden.has(n.id))
      .filter((n) => !query.get('kind') || n.kind === query.get('kind'))
      .sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'folder' ? -1 : 1))
    return page(items, query)
  }),
  route('post', '/v1/nodes/{node_id}/rename', ({ params, body, tenant }) => {
    const n = findNode(params.node_id, tenant)
    if (!body.name.trim() || /[/\\]/.test(body.name)) throw new Fail(422, 'validation_failed', 'Names cannot be empty or contain slashes.')
    const from = n.name
    n.name = placeName(n.parent_id!, body.name, tenant, body.on_conflict, n.id)
    touch(n)
    return { node: n, change: record('node.renamed', n.id, tenant, { from, to: n.name }) }
  }),
  route('post', '/v1/nodes/{node_id}/move', ({ params, body, tenant }) => {
    const n = findNode(params.node_id, tenant)
    const target = findNode(body.parent_id, tenant)
    if (target.kind !== 'folder') throw new Fail(422, 'validation_failed', 'The target is not a folder.')
    if (isInside(target.id, n.id, tenant)) throw new Fail(409, 'conflict', 'A folder cannot move into itself.')
    n.name = placeName(target.id, body.name ?? n.name, tenant, body.on_conflict, n.id)
    n.parent_id = target.id
    touch(n)
    return { node: n, change: record('node.moved', n.id, tenant, { to: target.id }) }
  }),
  route('post', '/v1/nodes/{node_id}/trash', ({ params, tenant }) => {
    const n = findNode(params.node_id, tenant)
    if (!n.parent_id) throw new Fail(409, 'conflict', 'The root cannot go to the trash.')
    n.trashed_at = now()
    touch(n)
    return { node: n, change: record('node.trashed', n.id, tenant) }
  }),
  route('post', '/v1/nodes/{node_id}/restore', ({ params, body, tenant }) => {
    const n = findNode(params.node_id, tenant)
    const parentId = body?.parent_id ?? n.parent_id!
    n.name = placeName(parentId, n.name, tenant, body?.on_conflict ?? 'rename', n.id)
    n.parent_id = parentId
    n.trashed_at = null
    touch(n)
    return { node: n, change: record('node.restored', n.id, tenant) }
  }),
  route('get', '/v1/trash', ({ tenant, query }) => {
    requireTenantHeader(tenant)
    return page(db.nodes.filter((n) => n.tenant_id === tenant && n.trashed_at), query)
  }),
  route('post', '/v1/trash/empty', ({ tenant }) => {
    const trashed = db.nodes.filter((n) => n.tenant_id === tenant && n.trashed_at)
    db.nodes = db.nodes.filter((x) => !trashed.some((t) => isInside(x.id, t.id, tenant)))
    return { purged: trashed.length, change: trashed.length ? change() : null }
  }),
  route(
    'put',
    '/v1/nodes/{node_id}/content',
    async ({ params, request, tenant }) => {
      const n = findNode(params.node_id, tenant)
      if (n.kind !== 'file') throw new Fail(422, 'validation_failed', 'Only files have content.')
      const text = await request.text()
      const version: Schemas['Version'] = {
        id: newId(),
        tenant_id: tenant,
        node_id: n.id,
        size_bytes: new TextEncoder().encode(text).length,
        content_hash: `sha256:${newId().replace(/-/g, '')}`,
        content_type: request.headers.get('Content-Type'),
        restored_from: null,
        label: null,
        actor: userRef(),
        on_behalf_of: null,
        created_at: now(),
      }
      db.versions.push(version)
      db.contents.set(version.id, text)
      n.head_version = version.id
      n.size_bytes = version.size_bytes
      touch(n)
      return { node: n, version, change: record('version.created', n.id, tenant) }
    },
    { rawBody: true },
  ),
  route('get', '/v1/nodes/{node_id}/content-url', ({ params, query, tenant }) => {
    const n = findNode(params.node_id, tenant)
    const versionId = query.get('version') ?? n.head_version
    if (!versionId) throw notFound('Content')
    const text = db.contents.get(versionId) ?? ''
    return {
      url: `data:text/plain;charset=utf-8;base64,${base64(text)}`,
      expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
      version_id: versionId,
    }
  }),
  route('get', '/v1/nodes/{node_id}/versions', ({ params, tenant, query }) => {
    findNode(params.node_id, tenant)
    return page(
      db.versions.filter((v) => v.node_id === params.node_id).sort((a, b) => b.created_at.localeCompare(a.created_at)),
      query,
    )
  }),
  route('post', '/v1/nodes/{node_id}/versions/{version_id}/restore', ({ params, tenant }) => {
    const n = findNode(params.node_id, tenant)
    const old = db.versions.find((v) => v.id === params.version_id && v.node_id === n.id)
    if (!old) throw notFound('Version')
    const version: Schemas['Version'] = { ...old, id: newId(), restored_from: old.id, label: null, actor: userRef(), on_behalf_of: null, created_at: now() }
    db.versions.push(version)
    db.contents.set(version.id, db.contents.get(old.id) ?? '')
    n.head_version = version.id
    n.size_bytes = version.size_bytes
    touch(n)
    return { node: n, version, change: record('version.restored', n.id, tenant, { from: old.id }) }
  }),
  route('put', '/v1/nodes/{node_id}/versions/{version_id}/label', ({ params, body, tenant }) => {
    findNode(params.node_id, tenant)
    const version = db.versions.find((v) => v.id === params.version_id && v.node_id === params.node_id)
    if (!version) throw notFound('Version')
    version.label = body.label
    return { version, change: record('version.labeled', params.node_id, tenant) }
  }),
  route('get', '/v1/nodes/{node_id}/diff', ({ params, query, tenant }) => {
    const n = findNode(params.node_id, tenant)
    const from = query.get('from')
    const to = query.get('to') ?? n.head_version
    if (!from || !to || !db.contents.has(from) || !db.contents.has(to)) throw notFound('Version')
    return { ...lineDiff(db.contents.get(from)!, db.contents.get(to)!), from, to }
  }),

  // ------------------------------------------------------------------ tags
  route('get', '/v1/tags', ({ tenant, query }) => page(db.tags.filter((t) => t.tenant_id === tenant), query)),
  route(
    'post',
    '/v1/tags',
    ({ body, tenant }) => {
      if (!body.name.trim()) throw new Fail(422, 'validation_failed', 'Give the tag a name.')
      if (db.tags.some((t) => t.tenant_id === tenant && t.name === body.name)) throw new Fail(409, 'name_taken', 'This tag exists already.')
      const tag: Schemas['Tag'] = { id: newId(), tenant_id: tenant, name: body.name, color: body.color ?? null, created_by: byUser(), created_at: now() }
      db.tags.push(tag)
      return { tag, change: change() }
    },
    { status: 201 },
  ),
  route('get', '/v1/nodes/{node_id}/tags', ({ params, tenant }) => {
    findNode(params.node_id, tenant)
    const tagIds = db.nodeTags.filter((x) => x.node_id === params.node_id).map((x) => x.tag_id)
    return { items: db.tags.filter((t) => tagIds.includes(t.id)) }
  }),
  route('put', '/v1/nodes/{node_id}/tags/{tag_id}', ({ params, tenant }) => {
    findNode(params.node_id, tenant)
    if (!db.tags.some((t) => t.id === params.tag_id && t.tenant_id === tenant)) throw notFound('Tag')
    if (!db.nodeTags.some((x) => x.node_id === params.node_id && x.tag_id === params.tag_id)) {
      db.nodeTags.push({ node_id: params.node_id, tag_id: params.tag_id })
    }
    return { change: record('tag.attached', params.node_id, tenant, { tag_id: params.tag_id }) }
  }),
  route('delete', '/v1/nodes/{node_id}/tags/{tag_id}', ({ params, tenant }) => {
    findNode(params.node_id, tenant)
    db.nodeTags = db.nodeTags.filter((x) => !(x.node_id === params.node_id && x.tag_id === params.tag_id))
    return { change: record('tag.detached', params.node_id, tenant, { tag_id: params.tag_id }) }
  }),

  // ------------------------------------------------------------------ sharing
  route('get', '/v1/nodes/{node_id}/grants', ({ params, tenant, query }) => {
    findNode(params.node_id, tenant)
    return page(db.grants.filter((g) => g.node_id === params.node_id), query)
  }),
  route(
    'post',
    '/v1/nodes/{node_id}/grants',
    ({ params, body, tenant }) => {
      const n = findNode(params.node_id, tenant)
      if (!n.my_can_share) throw new Fail(403, 'permission_denied', 'You cannot share this node.')
      if (body.grantee.type === 'email' && !/^[^@\s]+@[^@\s]+$/.test(body.grantee.email)) {
        throw new Fail(422, 'validation_failed', 'Enter a valid email address.')
      }
      const known = body.grantee.type === 'email' ? db.members.find((m) => m.email === (body.grantee as Schemas['GranteeEmail']).email) : undefined
      const grantee: Schemas['Grantee'] = known
        ? { type: 'user', id: known.user_id, display_name: known.display_name, email: known.email }
        : body.grantee
      const grant: Schemas['Grant'] = { id: newId(), node_id: n.id, grantee, role: body.role, inherited_from: null, created_by: byUser(), created_at: now() }
      db.grants.push(grant)
      return { grant, change: record('grant.added', n.id, tenant) }
    },
    { status: 201 },
  ),
  route('patch', '/v1/nodes/{node_id}/grants/{grant_id}', ({ params, body, tenant }) => {
    const grant = db.grants.find((g) => g.id === params.grant_id && g.node_id === params.node_id)
    if (!grant) throw notFound('Grant')
    grant.role = body.role
    return { grant, change: record('grant.updated', params.node_id, tenant) }
  }),
  route('delete', '/v1/nodes/{node_id}/grants/{grant_id}', ({ params, tenant }) => {
    const before = db.grants.length
    db.grants = db.grants.filter((g) => !(g.id === params.grant_id && g.node_id === params.node_id))
    if (db.grants.length === before) throw notFound('Grant')
    return { change: record('grant.removed', params.node_id, tenant) }
  }),
  route('patch', '/v1/nodes/{node_id}/sharing', ({ params, body, tenant }) => {
    const n = findNode(params.node_id, tenant)
    n.editors_can_share = body.editors_can_share
    n.editors_can_share_effective = body.editors_can_share ?? true
    touch(n)
    return { node: n, change: record('node.sharing_updated', n.id, tenant) }
  }),
  route('get', '/v1/nodes/{node_id}/share-links', ({ params, tenant, query }) => {
    findNode(params.node_id, tenant)
    // Like the server, the list has the revoked links too (with `revoked_at`).
    return page(db.shareLinks.filter((l) => l.node_id === params.node_id), query)
  }),
  route(
    'post',
    '/v1/nodes/{node_id}/share-links',
    ({ params, body, tenant }) => {
      const n = findNode(params.node_id, tenant)
      const policy = membership(tenant).tenant.policy
      if (!policy.allow_share_links) throw new Fail(403, 'permission_denied', 'Share links are off in this tenant.')
      if (body.role === 'edit' && !policy.allow_edit_links) throw new Fail(403, 'permission_denied', 'Edit links are off in this tenant.')
      const token = newId().replace(/-/g, '')
      const share_link: Schemas['ShareLink'] = {
        id: newId(),
        node_id: n.id,
        role: body.role,
        token,
        url: `${typeof location === 'undefined' ? 'https://ruum.localhost' : location.origin}/s/${token}`,
        expires_at: body.expires_at ?? null,
        has_password: Boolean(body.password),
        created_by: byUser(),
        created_at: now(),
        revoked_at: null,
      }
      db.shareLinks.push(share_link)
      return { share_link, change: record('share_link.created', n.id, tenant) }
    },
    { status: 201 },
  ),
  route('delete', '/v1/share-links/{link_id}', ({ params, tenant }) => {
    const l = db.shareLinks.find((x) => x.id === params.link_id)
    if (!l) throw notFound('Share link')
    l.revoked_at = now()
    return { change: record('share_link.revoked', l.node_id, tenant) }
  }),
  // Across all tenants of the user; the operation takes no Ruum-Tenant header. Newest first.
  route('get', '/v1/shared-with-me', ({ query }) =>
    page([...db.shared].sort((a, b) => b.shared_at.localeCompare(a.shared_at)), query),
  ),

  // ------------------------------------------------------------------ search
  route('get', '/v1/search', ({ query, tenant }) => {
    const q = (query.get('q') ?? '').toLowerCase()
    if (!q) throw new Fail(422, 'validation_failed', 'Enter a search term.')
    const tag = query.get('tag')
    const hits = db.nodes
      .filter((n) => n.tenant_id === tenant && n.parent_id && !n.trashed_at && !db.hidden.has(n.id) && n.name.toLowerCase().includes(q))
      .filter((n) => !tag || db.nodeTags.some((x) => x.node_id === n.id && x.tag_id === tag))
      .map((node) => ({ node, path: pathOf(node, tenant) }))
    return page(hits, query)
  }),
]
