/**
 * Reads `client_id` from the `request` value of `oauthAuthorize`, if it is a JWS (`a.b.c`).
 * The contract says only that `request` is signed and short-lived; the server checks it. If the UI
 * cannot read it, the screen still works, without the client's name.
 */
export function clientIdOfRequest(request: string): string | null {
  return claimOfRequest(request, 'client_id')
}

/**
 * Reads `redirect_uri` from the `request` value, if it is a JWS that carries it. The contract does
 * not promise this claim; without it the consent screen shows all registered redirect hosts.
 */
export function redirectUriOfRequest(request: string): string | null {
  return claimOfRequest(request, 'redirect_uri')
}

function claimOfRequest(request: string, claim: string): string | null {
  const part = request.split('.')[1]
  if (!part) return null
  try {
    const json = atob(part.replace(/-/g, '+').replace(/_/g, '/'))
    const payload = JSON.parse(json) as Record<string, unknown>
    const value = payload[claim]
    return typeof value === 'string' ? value : null
  } catch {
    return null
  }
}
