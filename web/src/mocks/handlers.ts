// Hand-written MSW handlers for the operations the web UI uses. Paths, bodies and answers are typed
// by the contract (src/api/schema.d.ts), so a contract change breaks the build here first.

import { http, HttpResponse, type HttpHandler } from 'msw'
import { blake3Hex } from '../upload/chunker'
import type { paths, Schemas } from '../api/client'
import { type CreateUploadRequest, type Upload } from '../api/uploads'
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
  /** The share link of a link bearer (`Authorization: Bearer mocklink.<id>`). */
  link?: Schemas['ShareLink']
}

/** An error answer with the standard error body. Throw it from a resolver. */
class Fail {
  readonly status: number
  readonly code: Schemas['ErrorCode']
  readonly message: string
  readonly hint?: string
  constructor(status: number, code: Schemas['ErrorCode'], message: string, hint?: string) {
    this.status = status
    this.code = code
    this.message = message
    this.hint = hint
  }
}

const notFound = (what: string) => new Fail(404, 'not_found', `${what} not found.`)

type Options = {
  status?: number
  public?: boolean
  rawBody?: boolean
  /** The bearer of a share link may call it (node, children and content-url reads). */
  link?: boolean
}

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
      // A share link's access token from resolveShareLink: only the reads of the link's subtree.
      const auth = request.headers.get('Authorization') ?? ''
      const linkId = auth.startsWith('Bearer mocklink.') ? auth.slice('Bearer mocklink.'.length) : null
      const link = linkId ? db.shareLinks.find((l) => l.id === linkId && !l.revoked_at) : undefined
      if (linkId) {
        if (!link) throw new Fail(401, 'unauthenticated', 'The link token does not work.')
        if (!options.link) throw new Fail(403, 'user_required', 'Share links cannot call this operation.')
      } else if (!options.public && !db.signedIn) throw new Fail(401, 'unauthenticated', 'Sign in first.')
      let body: unknown = undefined
      if (!options.rawBody && method !== 'get' && method !== 'delete') {
        const text = await request.clone().text()
        body = text ? JSON.parse(text) : undefined
      }
      // Cookie sessions: a state-changing request must send `Ruum-Csrf` with the value of the cookie
      // `ruum_csrf` (double submit).
      if (method !== 'get' && !options.public && !link) {
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
        link,
      })
      if (result === undefined) return new HttpResponse(null, { status: 204 })
      return HttpResponse.json(result as never, { status: options.status ?? 200 })
    } catch (e) {
      if (e instanceof Fail) {
        return HttpResponse.json({ error: { code: e.code, message: e.message, ...(e.hint ? { hint: e.hint } : {}) } }, { status: e.status })
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

/** For a link bearer: the node must be the link's node or inside it. */
function checkLink(link: Schemas['ShareLink'] | undefined, n: Schemas['Node']) {
  if (!link) return
  for (let cur: Schemas['Node'] | undefined = n; cur; cur = db.nodes.find((x) => x.id === cur!.parent_id)) {
    if (cur.id === link.node_id) return
  }
  throw notFound('Node')
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

function sameName(a: string, b: string) {
  return a.normalize('NFC').toLowerCase() === b.normalize('NFC').toLowerCase()
}

function nameTaken(parentId: string | null, name: string, tenant: string, except?: string) {
  return db.nodes.some(
    // Like the server: NFC and case-insensitive (`Plan.txt` and `plan.txt` clash).
    (n) => n.tenant_id === tenant && n.parent_id === parentId && sameName(n.name, name) && !n.trashed_at && n.id !== except,
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

/** The words that changed inside one line, as the `detail` of a `modify` change. */
function wordDetail(a: string, b: string): Schemas['DiffChange'][] {
  const x = a.split(/(\s+)/)
  const y = b.split(/(\s+)/)
  let head = 0
  while (head < x.length && head < y.length && x[head] === y[head]) head++
  let tail = 0
  while (tail < x.length - head && tail < y.length - head && x[x.length - 1 - tail] === y[y.length - 1 - tail]) tail++
  const old = x.slice(head, x.length - tail).join('')
  const neu = y.slice(head, y.length - tail).join('')
  if (!old) return [{ kind: 'insert', new: neu }]
  if (!neu) return [{ kind: 'delete', old }]
  return [{ kind: 'replace', old, new: neu }]
}

/** A naive line diff (the `text` plugin of the mock), enough for the versions panel. */
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
    const where = { path: `lines/${i + 1}`, label: `Line ${i + 1}` }
    if (o === undefined) {
      changes.push({ kind: 'insert', ...where, new: n })
      summary.inserted++
    } else if (n === undefined) {
      changes.push({ kind: 'delete', ...where, old: o })
      summary.deleted++
    } else {
      changes.push({ kind: 'modify', ...where, old: o, new: n, detail: wordDetail(o, n) })
      summary.modified++
    }
  }
  return { plugin: 'text@1.0.0', summary, changes, truncated: false }
}

/** The `binary` plugin of the mock: one change when the bytes differ. */
function binaryDiff(from: Schemas['Version'], to: Schemas['Version']): Schemas['DiffResult'] {
  const same = from.content_hash === to.content_hash
  return {
    plugin: 'binary@1.0.0',
    summary: { inserted: 0, deleted: 0, modified: same ? 0 : 1, moved: 0 },
    changes: same ? [] : [{ kind: 'replace', path: '', label: 'Content', old: `${from.size_bytes} bytes`, new: `${to.size_bytes} bytes` }],
    truncated: false,
  }
}

const diffPlugins: Schemas['DiffPluginInfo'][] = [
  { name: 'text', version: '1.0.0', priority: 100 },
  { name: 'binary', version: '1.0.0', priority: 0 },
]

/** The chunking parameter sets of the mock: small chunks for small and text files, as the server. */
const SMALL_CHUNKING: Schemas['ChunkingParams'] = {
  id: 'fastcdc-2020-16k-32k-64k',
  algorithm: 'fastcdc-2020',
  min_size: 16384,
  avg_size: 32768,
  max_size: 65536,
}
const DEFAULT_CHUNKING: Schemas['ChunkingParams'] = {
  id: 'fastcdc-2020-1m-2m-4m',
  algorithm: 'fastcdc-2020',
  min_size: 1048576,
  avg_size: 2097152,
  max_size: 4194304,
}
const chunkingSets = [SMALL_CHUNKING, DEFAULT_CHUNKING]

function chunkingFor(size: number, contentType: string | null): Schemas['ChunkingParams'] {
  return size <= 4 * 1024 * 1024 || contentType?.startsWith('text/') ? SMALL_CHUNKING : DEFAULT_CHUNKING
}

/** Makes a new head version of a file with this content. */
function writeVersion(n: Schemas['Node'], tenant: string, bytes: Uint8Array, contentType: string | null) {
  const version: Schemas['Version'] = {
    id: newId(),
    tenant_id: tenant,
    node_id: n.id,
    size_bytes: bytes.length,
    content_hash: blake3Hex(bytes),
    content_type: contentType,
    restored_from: null,
    label: null,
    actor: userRef(),
    on_behalf_of: null,
    created_at: now(),
  }
  db.versions.push(version)
  db.contents.set(version.id, new TextDecoder().decode(bytes))
  n.head_version = version.id
  n.size_bytes = version.size_bytes
  touch(n)
  return version
}

function canEditNode(n: Schemas['Node']) {
  return n.my_role === 'edit' || n.my_role === 'manage'
}

function findUpload(id: string, tenant: string): Upload {
  const u = db.uploads.get(id)
  if (!u || u.tenant_id !== tenant || u.status !== 'open') throw notFound('Upload')
  return u
}

const missingChunks = (u: Upload) =>
  [...new Set(u.chunks.map((c) => c.hash))].filter((h) => !db.chunks.has(`${u.tenant_id}:${h}`))

export const handlers: HttpHandler[] = [
  // ------------------------------------------------------------------ auth and account
  route('post', '/v1/auth/logout', () => {
    db.signedIn = false
    if (typeof document !== 'undefined') document.cookie = 'ruum_mock_session=; Max-Age=0; Path=/'
    return { logout_url: '/signin?signed_out=1' }
  }),
  route('get', '/v1/me', () => ({
    actor: userRef(),
    on_behalf_of: null,
    user: db.user,
    agent: null,
    bound_tenant_id: null,
    preferences: { ...db.preferences },
  })),
  route('patch', '/v1/me/preferences', ({ body }) => {
    if (body.locale !== undefined) db.preferences = { locale: body.locale }
    return { ...db.preferences }
  }),
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
      return {
        app_password,
        secret: `${app_password.prefix}${secretChars(39)}`,
        dav_url: 'https://dav.ruum.localhost/',
      }
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
      db.limits[id] = { ...db.limits[ids.team], usage: { storage_bytes: 0, nodes: 1, agents: 0, members: 1 } }
      return tenant
    },
    { status: 201 },
  ),
  route('get', '/v1/tenants/{tenant_id}', ({ params }) => membership(params.tenant_id).tenant),
  route('patch', '/v1/tenants/{tenant_id}', ({ params, body }) => {
    const m = requireAdmin(params.tenant_id)
    // The address part of a personal space's apps (card S-35): once, alone, by the rules.
    if (body.app_slug !== undefined) {
      const slug = body.app_slug
      if (body.name !== undefined || body.policy) throw new Fail(422, 'validation_failed', 'send app_slug in a request of its own', '/app_slug')
      if (m.tenant.kind !== 'personal') throw new Fail(422, 'validation_failed', 'only a personal space chooses the address of its apps', '/app_slug')
      if (!/^(?!xn-)[a-z0-9](?:[a-z0-9]|-(?!-)){1,38}[a-z0-9]$/.test(slug) || ['www', 'api', 'admin', 'app', 'apps'].includes(slug))
        throw new Fail(422, 'validation_failed', 'this slug breaks a rule', '/app_slug')
      if (m.tenant.app_slug && m.tenant.app_slug !== slug) throw new Fail(409, 'conflict', 'the address is fixed already', 'app_slug_fixed')
      if (db.tenants.some((x) => x.tenant.id !== m.tenant.id && x.tenant.app_slug === slug)) throw new Fail(409, 'conflict', 'this address is taken', 'app_slug_taken')
      Object.assign(m.tenant, { app_slug: slug })
      return m.tenant
    }
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
    // The content role applies only to members (decision 0003-1): another role clears it, a member
    // who stays a member keeps it when it is left out, and it is refused with another role.
    if (body.content_role && body.role !== 'member') throw new Fail(422, 'validation_failed', 'content_role needs role member.')
    m.content_role = body.role === 'member' ? (body.content_role ?? (m.role === 'member' ? (m.content_role ?? 'manage') : 'manage')) : null
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
        content_role: body.role === 'member' ? (body.content_role ?? 'manage') : null,
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
  route(
    'post',
    '/v1/invitations/preview',
    ({ body }) => {
      const p = db.invitationPreviews.get(body.token)
      if (!p) throw notFound('Invitation')
      return p
    },
    { public: true },
  ),
  route('post', '/v1/invitations/accept', ({ body }) => {
    if (!db.invitationTokens.has(body.token)) throw notFound('Invitation')
    db.invitationTokens.delete(body.token)
    db.invitationPreviews.set(body.token, { state: 'accepted' })
    const id = newId()
    const root = newId()
    const tenant: Schemas['Tenant'] = {
      id,
      name: 'Lovelace Lab',
      kind: 'team',
      plan: 'team',
      region: 'hel1',
      root_node_id: root,
      policy: { allow_share_links: true, allow_edit_links: true, share_link_max_days: null, allow_guests: true, members_can_share: true },
      created_at: now(),
    }
    db.tenants.push({ tenant, role: 'member' })
    db.nodes.push({ ...db.nodes.find((n) => n.id === ids.teamRoot)!, id: root, tenant_id: id, my_role: 'manage' })
    const member: Schemas['Member'] = { tenant_id: id, user_id: ids.ada, email: db.user.email, display_name: db.user.display_name, role: 'member', joined_at: now() }
    db.members.push(member)
    return member
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
    const entries = db.audit
      .filter((a) => (!a.tenant_id || a.tenant_id === params.tenant_id) && (!action || a.action.startsWith(action)))
      .sort((x, y) => y.at.localeCompare(x.at))
      .map((a) => {
        const copy: Partial<typeof a> = { ...a }
        delete copy.tenant_id
        return copy as Schemas['AuditEntry']
      })
    return page(entries, query)
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
    const item = (j: Schemas['JournalEntry'], reason: Schemas['UndoItem']['reason'], changedBy?: Schemas['ActorPair']): Schemas['UndoItem'] => ({
      seq: j.seq,
      node_id: j.node_id,
      type: j.type,
      reason,
      ...(changedBy ? { changed_by: changedBy } : {}),
    })
    // Who changed the node first after the entry (C-11): a later journal entry of another actor,
    // or the person whose undo reverted the entry before (the undo was a later change).
    const changedBy = (j: Schemas['JournalEntry']): Schemas['ActorPair'] | undefined => {
      const later = db.journal.find((x) => x.tenant_id === j.tenant_id && x.node_id === j.node_id && x.seq > j.seq && x.actor.id !== j.actor.id)
      if (later) return { actor: later.actor, on_behalf_of: later.on_behalf_of }
      return db.undoneSeqs.has(j.seq) ? byUser() : undefined
    }
    // The mock treats a purge as irreversible, an entry that someone changed later as changed
    // later, and everything else as revertible. Like the server, the journal keeps the entries:
    // the activity still lists them.
    const reverted = entries.filter((j) => j.type !== 'node.purged' && !changedBy(j)).map((j) => item(j, null))
    const skipped = entries
      .filter((j) => j.type === 'node.purged' || changedBy(j))
      .map((j) => (j.type === 'node.purged' ? item(j, 'irreversible') : item(j, 'changed_later', changedBy(j))))
    if (body.dry_run) return { dry_run: true, reverted, skipped, change: null }
    for (const r of reverted) db.undoneSeqs.add(r.seq)
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
  route(
    'get',
    '/v1/nodes/{node_id}',
    ({ params, tenant, link }) => {
      const n = findNode(params.node_id, tenant)
      checkLink(link, n)
      return link ? { ...n, my_role: link.role, my_can_share: false } : n
    },
    { link: true },
  ),
  route('delete', '/v1/nodes/{node_id}', ({ params, tenant }) => {
    const n = findNode(params.node_id, tenant)
    if (!n.trashed_at) throw new Fail(409, 'conflict', 'Move the node to the trash first.')
    db.nodes = db.nodes.filter((x) => !isInside(x.id, n.id, tenant))
    return { change: record('node.purged', n.id, tenant) }
  }),
  route(
    'get',
    '/v1/nodes/{node_id}/children',
    ({ params, tenant, query, link }) => {
      const parent = findNode(params.node_id, tenant)
      checkLink(link, parent)
      const items = db.nodes
        .filter((n) => n.tenant_id === tenant && n.parent_id === parent.id && !n.trashed_at && !db.hidden.has(n.id))
        .filter((n) => !query.get('kind') || n.kind === query.get('kind'))
        .sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'folder' ? -1 : 1))
        .map((n) => (link ? { ...n, my_role: link.role, my_can_share: false } : n))
      return page(items, query)
    },
    { link: true },
  ),
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
      const bytes = new Uint8Array(await request.arrayBuffer())
      const version = writeVersion(n, tenant, bytes, request.headers.get('Content-Type'))
      return { node: n, version, change: record('version.created', n.id, tenant) }
    },
    { rawBody: true },
  ),
  // ------------------------------------------------------------------ chunked uploads
  route('get', '/v1/uploads/params', ({ query, tenant }) => {
    requireTenantHeader(tenant)
    const size = Number(query.get('size'))
    if (!Number.isInteger(size) || size < 0) throw new Fail(422, 'validation_failed', 'size must be a whole number of bytes.')
    return chunkingFor(size, query.get('content_type'))
  }),
  route(
    'post',
    '/v1/uploads',
    ({ body: generated, tenant }) => {
      requireTenantHeader(tenant)
      const body = generated as unknown as CreateUploadRequest
      const chunking = chunkingSets.find((c) => c.id === body.chunking)
      if (!chunking) throw new Fail(422, 'validation_failed', `Unknown chunking ${body.chunking}.`)
      if (body.chunks.reduce((n, c) => n + c.size, 0) !== body.size) {
        throw new Fail(422, 'validation_failed', 'The chunk sizes do not add up to the size.')
      }
      const target = body.target === 'new_file' ? findNode(body.parent_id, tenant) : findNode(body.node_id, tenant)
      if (body.target === 'new_file' && target.kind !== 'folder') throw new Fail(422, 'validation_failed', 'The parent is not a folder.')
      if (body.target === 'new_version' && target.kind !== 'file') throw new Fail(422, 'validation_failed', 'Only files have versions.')
      if (!canEditNode(target)) throw new Fail(403, 'permission_denied', 'You need edit access.')
      const upload: Upload = {
        ...body,
        id: newId(),
        tenant_id: tenant,
        content_type: body.content_type ?? null,
        missing: [] as string[],
        status: 'open' as const,
        chunking,
        expires_at: new Date(Date.now() + 24 * 3600_000).toISOString(),
        created_by: byUser(),
      }
      upload.missing = missingChunks(upload)
      db.uploads.set(upload.id, upload)
      return upload as unknown as Schemas['Upload']
    },
    { status: 201 },
  ),
  route('get', '/v1/uploads/{upload_id}', ({ params, tenant }) => {
    const u = findUpload(params.upload_id, tenant)
    return { ...u, missing: missingChunks(u) } as unknown as Schemas['Upload']
  }),
  route('delete', '/v1/uploads/{upload_id}', ({ params, tenant }) => {
    findUpload(params.upload_id, tenant).status = 'aborted'
  }),
  route(
    'put',
    '/v1/uploads/{upload_id}/chunks/{hash}',
    async ({ params, request, tenant }) => {
      const u = findUpload(params.upload_id, tenant)
      if (!u.chunks.some((c) => c.hash === params.hash)) throw new Fail(422, 'validation_failed', 'This chunk is not in the upload.')
      const bytes = new Uint8Array(await request.arrayBuffer())
      if (blake3Hex(bytes) !== params.hash) throw new Fail(422, 'hash_mismatch', 'The chunk does not match its hash.')
      db.chunks.set(`${tenant}:${params.hash}`, bytes)
    },
    { rawBody: true },
  ),
  route('post', '/v1/uploads/{upload_id}/complete', ({ params, tenant }) => {
    const u = findUpload(params.upload_id, tenant)
    const missing = missingChunks(u)
    if (missing.length) throw new Fail(409, 'chunk_missing', `${missing.length} chunks are missing.`)
    const bytes = new Uint8Array(u.size)
    let at = 0
    for (const c of u.chunks) {
      bytes.set(db.chunks.get(`${tenant}:${c.hash}`)!, at)
      at += c.size
    }
    if (blake3Hex(bytes) !== u.content_hash) throw new Fail(422, 'hash_mismatch', 'The content does not match content_hash.')
    let n: Schemas['Node']
    if (u.target === 'new_file') {
      const parent = findNode(u.parent_id, tenant)
      const name = placeName(parent.id, u.name, tenant, u.on_conflict)
      n = {
        ...parent,
        id: newId(),
        parent_id: parent.id,
        name,
        kind: 'file',
        type: null,
        head_version: null,
        size_bytes: 0,
        revision: 0,
        created_at: now(),
        created_by: byUser(),
        my_role: 'manage',
        my_can_share: true,
        editors_can_share: null,
      }
      db.nodes.push(n)
      record('node.created', n.id, tenant, { name })
    } else {
      n = findNode(u.node_id, tenant)
      if (u.base_version && u.base_version !== n.head_version) {
        throw new Fail(409, 'version_mismatch', 'The file has a newer version than the one this upload started from.')
      }
    }
    const version = writeVersion(n, tenant, bytes, u.content_type ?? null)
    u.status = 'completed'
    return { node: n, version, change: record('version.created', n.id, tenant) }
  }),
  route('get', '/v1/nodes/{node_id}/content-url', ({ params, query, tenant, link }) => {
    const n = findNode(params.node_id, tenant)
    checkLink(link, n)
    const versionId = query.get('version') ?? n.head_version
    if (!versionId) throw notFound('Content')
    const text = db.contents.get(versionId) ?? ''
    return {
      url: `data:text/plain;charset=utf-8;base64,${base64(text)}`,
      expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
      version_id: versionId,
    }
  }, { link: true }),
  route('get', '/v1/nodes/{node_id}/versions', ({ params, tenant, query }) => {
    findNode(params.node_id, tenant)
    return page(
      db.versions.filter((v) => v.node_id === params.node_id).sort((a, b) => b.created_at.localeCompare(a.created_at)),
      query,
    )
  }),
  route('get', '/v1/nodes/{node_id}/versions/{version_id}', ({ params, tenant }) => {
    findNode(params.node_id, tenant)
    const v = db.versions.find((x) => x.id === params.version_id && x.node_id === params.node_id)
    if (!v) throw notFound('Version')
    return v
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
    const vFrom = db.versions.find((v) => v.id === from && v.node_id === n.id)
    const vTo = db.versions.find((v) => v.id === to && v.node_id === n.id)
    if (!from || !to || !vFrom || !vTo) throw notFound('Version')
    const plugin = query.get('plugin') ?? 'text'
    if (!diffPlugins.some((p) => p.name === plugin)) throw new Fail(422, 'validation_failed', `Unknown diff plugin ${plugin}.`)
    const result = plugin === 'binary' ? binaryDiff(vFrom, vTo) : lineDiff(db.contents.get(from) ?? '', db.contents.get(to) ?? '')
    return { ...result, from, to }
  }),
  route('get', '/v1/nodes/{node_id}/diff/plugins', ({ params, tenant }) => {
    findNode(params.node_id, tenant)
    return { items: diffPlugins }
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
      // Like the server (contract, addGrant): a grant to an email stays an email grant until an
      // invitation is accepted, also for the email of a member; a grant by user ID reaches only
      // members and guests of the tenant (decision 0002-1).
      let grantee: Schemas['Grantee'] = body.grantee
      if (body.grantee.type === 'user') {
        const id = body.grantee.id
        const m = db.members.find((x) => x.tenant_id === tenant && x.user_id === id)
        if (!m) throw notFound('User')
        grantee = { type: 'user', id: m.user_id }
      }
      // Decision 0003-2 (a): a member, admin or owner who shares with the email of a person of the
      // space gets a grant to that user; (a') a guest always gets an email grant.
      const caller = db.tenants.find((x) => x.tenant.id === tenant)?.role
      if (body.grantee.type === 'email' && caller && caller !== 'guest') {
        const email = body.grantee.email.toLowerCase()
        const m = db.members.find((x) => x.tenant_id === tenant && x.email.toLowerCase() === email)
        if (m) grantee = { type: 'user', id: m.user_id, display_name: m.display_name, email: m.email }
      }
      if (body.message && body.message.length > 1000) throw new Fail(422, 'validation_failed', 'The message is too long.')
      const grant: Schemas['Grant'] = { id: newId(), node_id: n.id, grantee, role: body.role, inherited_from: null, created_by: byUser(), created_at: now() }
      db.grants.push(grant)
      // A share email goes out with `notify` (the default), except for everyone in the space or the
      // caller's own address, and at most once per recipient and item in 10 minutes.
      const to = grantee.type === 'email' ? grantee.email : grantee.type === 'user' ? (db.members.find((x) => x.user_id === grantee.id)?.email ?? null) : null
      const key = `${n.id}/${to}`
      const notified = body.notify !== false && to !== null && to !== db.user.email && !db.mailed.has(key)
      if (notified) db.mailed.add(key)
      return { grant, change: record('grant.added', n.id, tenant, { grant_id: grant.id, grantee, role: body.role }), notified }
    },
    { status: 201 },
  ),
  route('patch', '/v1/nodes/{node_id}/grants/{grant_id}', ({ params, body, tenant }) => {
    const grant = db.grants.find((g) => g.id === params.grant_id && g.node_id === params.node_id)
    if (!grant) throw notFound('Grant')
    const old = grant.role
    grant.role = body.role
    return { grant, change: record('grant.updated', params.node_id, tenant, { grant_id: grant.id, grantee: grant.grantee, old_role: old, new_role: body.role }) }
  }),
  route('delete', '/v1/nodes/{node_id}/grants/{grant_id}', ({ params, tenant }) => {
    const before = db.grants.length
    db.grants = db.grants.filter((g) => !(g.id === params.grant_id && g.node_id === params.node_id))
    if (db.grants.length === before) throw notFound('Grant')
    return { change: record('grant.removed', params.node_id, tenant) }
  }),
  route('patch', '/v1/nodes/{node_id}/sharing', ({ params, body, tenant }) => {
    const n = findNode(params.node_id, tenant)
    const old = n.editors_can_share ?? null
    n.editors_can_share = body.editors_can_share
    const parent = db.nodes.find((x) => x.id === n.parent_id)
    n.editors_can_share_effective = body.editors_can_share ?? parent?.editors_can_share_effective ?? true
    touch(n)
    return { node: n, change: record('node.sharing_updated', n.id, tenant, { old_editors_can_share: old, new_editors_can_share: body.editors_can_share }) }
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
      const token = `ruum_link_${secretChars(32)}`
      const id = newId()
      db.linkSecrets.set(token, { linkId: id, password: body.password })
      const share_link: Schemas['ShareLink'] = {
        id,
        node_id: n.id,
        role: body.role,
        token,
        // As the server: the token is in the fragment, so it never reaches a server log.
        url: `${typeof location === 'undefined' ? 'https://ruum.localhost' : location.origin}/s#${token}`,
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
  route(
    'post',
    '/v1/share-links/resolve',
    ({ body }) => {
      const secret = db.linkSecrets.get(body.token)
      const link = secret && db.shareLinks.find((l) => l.id === secret.linkId && !l.revoked_at)
      if (!secret || !link || (link.expires_at && link.expires_at < now())) throw notFound('Share link')
      if (link.has_password && body.password !== secret.password) throw new Fail(401, 'unauthenticated', 'The password is wrong.')
      const node = db.nodes.find((n) => n.id === link.node_id)!
      return {
        tenant_id: node.tenant_id,
        node: { ...node, my_role: link.role, my_can_share: false },
        role: link.role,
        access_token: `mocklink.${link.id}`,
        expires_in: 3600,
      }
    },
    { public: true },
  ),
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
