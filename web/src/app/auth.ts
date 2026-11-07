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

/** Where the accept page keeps an invitation token of the fragment while the person signs in. */
export const INVITATION_TOKEN_KEY = 'ruum.invitationToken'

/** The invitation token of an accept link (`#token=`), from the fragment, which never reaches a server. */
export function fragmentToken(hash: string): string | null {
  return new URLSearchParams(hash.replace(/^#/, '')).get('token')
}

/**
 * The sign-in page of the UI for the current location. The fragment never goes into `return_to`:
 * an invitation token of the fragment waits in this tab's session storage instead.
 */
export function signInPath(location: { pathname: string; search: string; hash?: string }): string {
  const token = location.hash ? fragmentToken(location.hash) : null
  if (token && location.pathname === '/invitations/accept') {
    try {
      sessionStorage.setItem(INVITATION_TOKEN_KEY, token)
    } catch {
      // Storage can be off: the person then opens the link of the email again after sign-in.
    }
  }
  const here = location.pathname + location.search
  return here.startsWith('/signin') ? '/signin' : `/signin?return_to=${encodeURIComponent(here)}`
}
