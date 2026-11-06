import type { paths } from '../api/client'

type LoginQuery = NonNullable<paths['/v1/auth/login']['get']['parameters']['query']>
export type Idp = NonNullable<LoginQuery['idp']>

/** The contract allows only relative paths in the UI (`^/[^/\\]`) as `return_to`. */
export function safeReturnTo(value: string | null | undefined): string {
  return value && /^\/[^/\\]/.test(value) ? value : '/'
}

/**
 * The URL of `loginStart`. The browser goes there with a full page load; the server redirects to
 * the identity provider, which comes back to `loginCallback`, which sets the session cookie and
 * redirects to `return_to`.
 */
export function loginUrl(returnTo: string, idp?: Idp): string {
  const q = new URLSearchParams({ return_to: safeReturnTo(returnTo) })
  if (idp) q.set('idp', idp)
  return `/v1/auth/login?${q}`
}

/** The sign-in page of the UI for the current location. */
export function signInPath(location: { pathname: string; search: string }): string {
  const here = location.pathname + location.search
  return here.startsWith('/signin') ? '/signin' : `/signin?return_to=${encodeURIComponent(here)}`
}
