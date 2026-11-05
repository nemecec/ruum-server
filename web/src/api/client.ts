// A small typed fetch helper over the generated contract types (src/api/schema.d.ts).
// Regenerate the types with `npm run gen:api` after a contract change; never edit them by hand.

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

function readCookie(name: string): string | undefined {
  return document.cookie
    .split('; ')
    .find((c) => c.startsWith(`${name}=`))
    ?.slice(name.length + 1)
}

/** Calls the API with the session cookie, the tenant header and the CSRF header. */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = new Headers(init.headers)
  headers.set('Accept', 'application/json')
  if (currentTenant) headers.set('Ruum-Tenant', currentTenant)
  const method = (init.method ?? 'GET').toUpperCase()
  if (method !== 'GET' && method !== 'HEAD') {
    const csrf = readCookie('ruum_csrf')
    if (csrf) headers.set('Ruum-Csrf', csrf)
  }
  const res = await fetch(path, { ...init, headers, credentials: 'same-origin' })
  const text = await res.text()
  const body: unknown = text ? JSON.parse(text) : undefined
  if (!res.ok) {
    const err = (body as Schemas['Error'] | undefined)?.error ?? {
      code: 'internal' as const,
      message: `HTTP ${res.status}`,
    }
    throw new RuumApiError(res.status, err)
  }
  return body as T
}
