// The laptop Zitadel (deploy/dev), through its management API as the bootstrap service account
// that scripts/dev.sh made. Used by the global setup only.
import { env } from './env.ts'

async function zapi<T>(method: string, path: string, body?: unknown): Promise<T> {
  const z = env.zitadel()
  const res = await fetch(`${z.issuer}${path}`, {
    method,
    headers: { Authorization: `Bearer ${z.pat}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await res.text()
  if (!res.ok) throw new Error(`Zitadel ${method} ${path}: ${res.status} ${text.slice(0, 300)}`)
  return (text ? JSON.parse(text) : {}) as T
}

interface OidcConfig {
  clientId: string
  redirectUris?: string[]
  postLogoutRedirectUris?: string[]
  [k: string]: unknown
}

/**
 * Makes sure the `ruum-web` client accepts the callback of the web UI under test
 * (`<web>/auth/callback`). scripts/dev.sh registers port 5173 only. Idempotent.
 */
export async function ensureRedirectUri(webUrl: string): Promise<void> {
  const z = env.zitadel()
  const apps = await zapi<{ result?: { id: string; oidcConfig?: OidcConfig }[] }>(
    'POST',
    `/management/v1/projects/${z.projectId}/apps/_search`,
    {},
  )
  const app = apps.result?.find((a) => a.oidcConfig?.clientId === z.webClientId)
  if (!app?.oidcConfig) throw new Error(`no OIDC app with client ID ${z.webClientId} in project ${z.projectId}`)
  const c = app.oidcConfig
  const origin = new URL(webUrl).origin
  const callback = `${origin}/auth/callback`
  const home = `${origin}/`
  const redirects = c.redirectUris ?? []
  const logouts = c.postLogoutRedirectUris ?? []
  if (redirects.includes(callback) && logouts.includes(home)) return
  await zapi('PUT', `/management/v1/projects/${z.projectId}/apps/${app.id}/oidc_config`, {
    redirectUris: [...new Set([...redirects, callback])],
    postLogoutRedirectUris: [...new Set([...logouts, home])],
    responseTypes: c.responseTypes,
    grantTypes: c.grantTypes,
    appType: c.appType,
    authMethodType: c.authMethodType,
    devMode: c.devMode,
    accessTokenType: c.accessTokenType,
    accessTokenRoleAssertion: c.accessTokenRoleAssertion,
    idTokenRoleAssertion: c.idTokenRoleAssertion,
    idTokenUserinfoAssertion: c.idTokenUserinfoAssertion,
    clockSkew: c.clockSkew,
    additionalOrigins: c.additionalOrigins,
  })
}

/** Makes a human user with a verified email and a password that need not change. */
export async function createUser(u: { email: string; password: string; givenName: string; familyName: string }): Promise<string> {
  const res = await zapi<{ userId: string }>('POST', '/v2/users/human', {
    username: u.email,
    profile: { givenName: u.givenName, familyName: u.familyName },
    email: { email: u.email, isVerified: true },
    password: { password: u.password, changeRequired: false },
  })
  return res.userId
}

/** The ID of the instance identity provider with this name on the login page, or undefined. */
export async function loginPolicyIdp(name: string): Promise<string | undefined> {
  const res = await zapi<{ result?: { idpId: string; idpName: string }[] }>('POST', '/admin/v1/policies/login/idps/_search', {})
  return res.result?.find((p) => p.idpName === name)?.idpId
}

/** The Zitadel user with this email and the identity providers linked to it. */
export async function userWithLinks(email: string): Promise<{ userId: string; emailVerified: boolean; links: { idpId: string; userId: string }[] } | undefined> {
  const users = await zapi<{ result?: { userId: string; human?: { email?: { email: string; isVerified?: boolean } } }[] }>('POST', '/v2/users', {
    queries: [{ emailQuery: { emailAddress: email } }],
  })
  const u = users.result?.[0]
  if (!u) return undefined
  const links = await zapi<{ result?: { idpId: string; userId: string }[] }>('POST', `/v2/users/${u.userId}/links/_search`, {})
  return { userId: u.userId, emailVerified: Boolean(u.human?.email?.isVerified), links: links.result ?? [] }
}
