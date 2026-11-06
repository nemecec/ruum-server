// The typed API client. All types come from the generated contract types (src/api/schema.d.ts).
// Regenerate them with `npm run gen:api` after a contract change; never edit them by hand.

import createClient, { type Middleware } from 'openapi-fetch'
import type { components, paths } from './schema'

export type Schemas = components['schemas']
export type ApiError = Schemas['Error']['error']
export type { paths }

/** An error answer of the API, with the standard error body. */
export class RuumApiError extends Error {
  readonly status: number
  readonly code: ApiError['code']
  readonly hint?: string

  constructor(status: number, body: ApiError) {
    super(body.message)
    this.name = 'RuumApiError'
    this.status = status
    this.code = body.code
    this.hint = body.hint
  }
}

/** The tenant of tenant-scoped calls (`Ruum-Tenant` header). */
let currentTenant: string | null = null

export function setTenant(tenantId: string | null): void {
  currentTenant = tenantId
}

export function getTenant(): string | null {
  return currentTenant
}

function readCookie(name: string): string | undefined {
  return document.cookie
    .split('; ')
    .find((c) => c.startsWith(`${name}=`))
    ?.slice(name.length + 1)
}

/**
 * The paths of the operations that declare the `TenantHeader` parameter in the contract. Only these
 * get the current tenant by default. User-level calls (`/v1/me`, `/v1/tenants`, OAuth consent) and
 * `listSharedWithMe` (which spans all tenants) must not carry it.
 */
const TENANT_PATHS = [
  /^\/v1\/nodes(\/|$)/,
  /^\/v1\/tags(\/|$)/,
  /^\/v1\/trash(\/|$)/,
  /^\/v1\/search(\/|$)/,
  /^\/v1\/agents(\/|$)/,
  /^\/v1\/uploads(\/|$)/,
  /^\/v1\/changes(\/|$)/,
  /^\/v1\/share-links\/(?!resolve$)[^/]+$/,
  /^\/v1\/me\/app-passwords(\/|$)/,
]

/** True when the operation at this path takes the `Ruum-Tenant` header. */
export function takesTenantHeader(pathname: string): boolean {
  return TENANT_PATHS.some((re) => re.test(pathname))
}

/** Adds the tenant header, the CSRF header and an idempotency key to each call. */
const ruumHeaders: Middleware = {
  onRequest({ request }) {
    request.headers.set('Accept', 'application/json')
    const pathname = new URL(request.url).pathname
    if (currentTenant && !request.headers.has('Ruum-Tenant') && takesTenantHeader(pathname)) {
      request.headers.set('Ruum-Tenant', currentTenant)
    }
    const method = request.method.toUpperCase()
    if (method !== 'GET' && method !== 'HEAD') {
      const csrf = readCookie('ruum_csrf')
      if (csrf) request.headers.set('Ruum-Csrf', csrf)
      if (!request.headers.has('Idempotency-Key')) {
        request.headers.set('Idempotency-Key', crypto.randomUUID())
      }
    }
    return request
  },
}

/**
 * The client. `fetch` is looked up on each call (not captured once), so the mock API of the tests
 * can replace it.
 */
export const client = createClient<paths>({
  baseUrl: typeof window === 'undefined' ? '' : window.location.origin,
  credentials: 'same-origin',
  fetch: (request: Request) => globalThis.fetch(request),
})
client.use(ruumHeaders)

/** Listeners for `401 unauthenticated`, so the app can send the user to sign-in. */
const unauthenticatedListeners = new Set<() => void>()

export function onUnauthenticated(listener: () => void): () => void {
  unauthenticatedListeners.add(listener)
  return () => unauthenticatedListeners.delete(listener)
}

/**
 * True while the person signs out. The server ends the session before the browser leaves for the
 * identity provider's end-session page, so calls still under way answer 401. They must not send
 * the browser to sign-in (with a `return_to`) at the same time as that navigation.
 */
let signingOut = false

export function setSigningOut(value: boolean) {
  signingOut = value
}

type Result<T> = { data?: T; error?: unknown; response: Response }

/**
 * Returns the data of a call, or throws a `RuumApiError` with the standard error body.
 * Use it as `await unwrap(client.GET('/v1/me'))`.
 */
export async function unwrap<T>(call: Promise<Result<T>>): Promise<T> {
  const { data, error, response } = await call
  if (!response.ok) {
    const body = (error as Schemas['Error'] | undefined)?.error ?? {
      code: response.status === 401 ? ('unauthenticated' as const) : ('internal' as const),
      message: `HTTP ${response.status}`,
    }
    if (response.status === 401 && !signingOut) unauthenticatedListeners.forEach((l) => l())
    throw new RuumApiError(response.status, body)
  }
  return data as T
}

/**
 * A client for a visitor of a share link: it sends the link's access token as Bearer, no cookies,
 * no CSRF header. The token stays in memory; it never goes into a URL.
 */
export function linkClient(accessToken: string, tenantId: string) {
  const c = createClient<paths>({
    baseUrl: typeof window === 'undefined' ? '' : window.location.origin,
    credentials: 'omit',
    fetch: (request: Request) => globalThis.fetch(request),
  })
  c.use({
    onRequest({ request }) {
      request.headers.set('Accept', 'application/json')
      request.headers.set('Authorization', `Bearer ${accessToken}`)
      if (takesTenantHeader(new URL(request.url).pathname) && !request.headers.has('Ruum-Tenant')) {
        request.headers.set('Ruum-Tenant', tenantId)
      }
      return request
    },
  })
  return c
}
