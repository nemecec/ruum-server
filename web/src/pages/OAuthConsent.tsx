import { useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'
import { client, unwrap, type Schemas } from '../api/client'
import { useApi } from '../api/useApi'
import { browser } from '../app/browser'
import { clientIdOfRequest, redirectUriOfRequest } from '../app/oauth'
import { useSession } from '../app/useSession'
import { roleLabels } from '../components/format'
import { ErrorNote, Loaded } from '../components/ui'
import { ScopeFields } from './agents/ScopeFields'

/** The host of a URL, or `null` for a malformed value (client metadata is not trusted). */
function hostOf(url: string | null | undefined): string | null {
  if (!url) return null
  try {
    return new URL(url).host
  } catch {
    return null
  }
}

/** A safe link target: only http(s) URLs. */
function safeHref(url: string): string | undefined {
  try {
    const u = new URL(url)
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.href : undefined
  } catch {
    return undefined
  }
}

/** The OAuth consent screen: `/oauth/consent?request=...`. Calls `oauthConsent`. */
export function OAuthConsentPage() {
  const [params] = useSearchParams()
  const request = params.get('request') ?? ''
  const { me, tenants } = useSession()
  const clientId = request ? clientIdOfRequest(request) : null
  const requestRedirect = request ? redirectUriOfRequest(request) : null
  const oauthClient = useApi(
    () =>
      clientId
        ? unwrap(client.GET('/v1/oauth/clients/{client_id}', { params: { path: { client_id: clientId } } }))
        : Promise.resolve(null),
    [clientId],
  )

  const [tenantId, setTenantId] = useState(me.user.home_tenant_id)
  const [mode, setMode] = useState<'new' | 'existing'>('new')
  const [agentId, setAgentId] = useState('')
  const [agentName, setAgentName] = useState('')
  const [scope, setScope] = useState<Schemas['AgentScope']>({ subtree: null, max_role: 'edit', expires_at: null })
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  const agents = useApi(
    () => unwrap(client.GET('/v1/agents', { params: { header: { 'Ruum-Tenant': tenantId } } })),
    [tenantId],
  )

  if (!request) {
    return (
      <main className="page narrow" id="main">
        <h1>Authorization request missing</h1>
        <p>Start the connection again from the application.</p>
      </main>
    )
  }

  const appName = oauthClient.data?.client_name ?? 'An application'

  async function answer(approve: boolean) {
    setBusy(true)
    setError(undefined)
    try {
      const body: Schemas['OAuthConsentRequest'] = approve
        ? {
            request,
            approve: true,
            tenant_id: tenantId,
            ...(mode === 'existing'
              ? { agent_id: agentId }
              : { new_agent: { name: agentName.trim() || appName, scope } }),
          }
        : { request, approve: false }
      const res = await unwrap(client.POST('/v1/oauth/consent', { body }))
      browser.assign(res.redirect_to)
    } catch (e) {
      setError(e)
      setBusy(false)
    }
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    void answer(true)
  }

  return (
    <main className="page narrow" id="main">
      <h1>Connect {appName} to Ruum</h1>
      <Loaded state={oauthClient}>
        {(c) => {
          if (!c) {
            return <p className="muted">Ruum could not show the name of the application. Continue only if you started this.</p>
          }
          const from = hostOf(c.client_uri)
          const href = c.client_uri ? safeHref(c.client_uri) : undefined
          // The redirect of this request when the UI can read it; otherwise the registered ones.
          const back = requestRedirect
            ? [hostOf(requestRedirect)]
            : c.redirect_uris.map((u) => hostOf(u))
          const backText = back.map((h) => h ?? 'an address that is not valid').join(', ')
          return (
            <div className="stack">
              <p className="warning" role="note">
                The application chose its name and web address itself. Ruum did not check them (unverified).
              </p>
              <p className="muted">
                {from ? (
                  <>
                    It says it is from {href ? <a href={href} rel="noreferrer noopener">{from}</a> : from}.{' '}
                  </>
                ) : null}
                {requestRedirect ? `It will go back to ${backText}.` : `It can go back to ${back.length > 1 ? 'one of ' : ''}${backText}.`}
              </p>
            </div>
          )
        }}
      </Loaded>
      <p>
        {appName} will act as an agent for you, <strong>{me.user.display_name}</strong>. It gets at most the access you choose, and
        never more than you have. You can disable it at any time on the Agents page.
      </p>
      <form className="stack" onSubmit={submit}>
        <label>
          Tenant
          <select
            value={tenantId}
            onChange={(e) => {
              setTenantId(e.target.value)
              setAgentId('')
              // A folder of one tenant is not valid in another.
              setScope((s) => ({ ...s, subtree: null }))
            }}
          >
            {tenants.map((t) => (
              <option key={t.tenant.id} value={t.tenant.id}>
                {t.tenant.name}
              </option>
            ))}
          </select>
        </label>
        <fieldset className="stack">
          <legend>Agent</legend>
          <label className="check">
            <input type="radio" name="mode" checked={mode === 'new'} onChange={() => setMode('new')} />
            Create a new agent
          </label>
          {mode === 'new' && (
            <div className="indent stack">
              <label>
                Agent name
                <input value={agentName} placeholder={appName} onChange={(e) => setAgentName(e.target.value)} />
              </label>
              <ScopeFields tenantId={tenantId} scope={scope} onChange={setScope} idPrefix="consent" />
            </div>
          )}
          <label className="check">
            <input
              type="radio"
              name="mode"
              checked={mode === 'existing'}
              disabled={!agents.data?.items.length}
              onChange={() => setMode('existing')}
            />
            Use an existing agent
          </label>
          {mode === 'existing' && agents.data && (
            <label className="indent">
              Existing agent
              <select required value={agentId} onChange={(e) => setAgentId(e.target.value)}>
                <option value="">Choose an agent</option>
                {agents.data.items
                  .filter((a) => !a.disabled_at)
                  .map((a) => (
                    <option key={a.id} value={a.id}>
                      {a.name} ({roleLabels[a.scope.max_role].toLowerCase()})
                    </option>
                  ))}
              </select>
            </label>
          )}
        </fieldset>
        <ErrorNote error={error} />
        <div className="actions">
          <button type="button" disabled={busy} onClick={() => answer(false)}>
            Deny
          </button>
          <button type="submit" className="primary" disabled={busy || (mode === 'existing' && !agentId)}>
            Allow
          </button>
        </div>
      </form>
    </main>
  )
}
