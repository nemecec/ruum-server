import { useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'
import { client, unwrap, type Schemas } from '../api/client'
import { useApi } from '../api/useApi'
import { browser } from '../app/browser'
import { clientIdOfRequest, redirectUriOfRequest } from '../app/oauth'
import { useSession } from '../app/useSession'
import { roleLabels } from '../components/format'
import { accessSummary } from './agents/status'
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
  // Defaults of journeys J5 step 3: only in a folder the person chooses, ending in 90 days.
  const [scope, setScope] = useState<Schemas['AgentScope']>(() => ({
    subtree: '',
    max_role: 'edit',
    expires_at: new Date(Date.now() + 90 * 86400_000).toISOString(),
  }))
  const [done, setDone] = useState(false)
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  const agents = useApi(
    () => unwrap(client.GET('/v1/agents', { params: { header: { 'Ruum-Tenant': tenantId } } })),
    [tenantId],
  )

  if (!request) {
    return (
      <main className="page narrow" id="main">
        <h1>This connection request has ended</h1>
        <p>Start the connection again from the app.</p>
      </main>
    )
  }

  const appName = oauthClient.data?.client_name ?? 'An app'
  const spaceName = tenants.find((t) => t.tenant.id === tenantId)?.tenant.name ?? 'your space'
  const folderMissing = mode === 'new' && scope.subtree === ''

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
      if (approve) setDone(true)
      browser.assign(res.redirect_to)
    } catch (e) {
      setError(e)
      setBusy(false)
    }
  }

  if (done) {
    return (
      <main className="page narrow" id="main">
        <h1>Connected</h1>
        <p>You can go back to {appName}.</p>
      </main>
    )
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    void answer(true)
  }

  return (
    <main className="page narrow" id="main">
      <h1>{appName} wants to connect to Ruum</h1>
      <Loaded state={oauthClient}>
        {(c) => {
          if (!c) {
            return <p className="warning" role="note">Ruum can't show the name of this app. Continue only if you started this.</p>
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
                Ruum can't check the name or address of this app. Continue only if you started this.
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
        It will act as an agent for you, <strong>{me.user.display_name}</strong>. It gets only the access you choose here, and never
        more than you have. You can pause it at any time on the Agents page.
      </p>
      <form className="stack" onSubmit={submit}>
        <label>
          Space
          <select
            value={tenantId}
            onChange={(e) => {
              setTenantId(e.target.value)
              setAgentId('')
              // A folder of one tenant is not valid in another.
              setScope((s) => ({ ...s, subtree: typeof s.subtree === 'string' ? '' : null }))
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
              <ScopeFields tenantId={tenantId} spaceName={spaceName} scope={scope} onChange={setScope} idPrefix="consent" />
              {!folderMissing && <p className="note">{accessSummary(scope, null, spaceName)}.</p>}
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
            Don't allow
          </button>
          <button type="submit" className="primary" disabled={busy || (mode === 'existing' && !agentId) || folderMissing}>
            Allow
          </button>
        </div>
        {folderMissing && <p className="muted small">Choose a folder first, or choose Everywhere.</p>}
      </form>
    </main>
  )
}
