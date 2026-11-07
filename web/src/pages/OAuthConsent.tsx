import { useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'
import { client, unwrap, type Schemas } from '../api/client'
import { useApi } from '../api/useApi'
import { browser } from '../app/browser'
import { clientIdOfRequest, redirectUriOfRequest } from '../app/oauth'
import { useSession } from '../app/useSession'
import { roleLabels } from '../components/format'
import { accessSummary } from './agents/status'
import { ScopeFields } from './agents/ScopeFields'
import { Actions, Button, ErrorNote, Field, Fieldset, Indent, Loaded, Note, Page, Radio, Select, Stack, Text, TextInput } from '../ui'

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
      clientId ? unwrap(client.GET('/v1/oauth/clients/{client_id}', { params: { path: { client_id: clientId } } })) : Promise.resolve(null),
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

  const agents = useApi(() => unwrap(client.GET('/v1/agents', { params: { header: { 'Ruum-Tenant': tenantId } } })), [tenantId])

  if (!request) {
    return (
      <Page narrow id="main">
        <h1>This connection request has ended</h1>
        <p>Start the connection again from the app.</p>
      </Page>
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
            ...(mode === 'existing' ? { agent_id: agentId } : { new_agent: { name: agentName.trim() || appName, scope } }),
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
      <Page narrow id="main">
        <h1>Connected</h1>
        <p>You can go back to {appName}.</p>
      </Page>
    )
  }

  function submit(e: FormEvent) {
    e.preventDefault()
    void answer(true)
  }

  return (
    <Page narrow id="main">
      <h1>{appName} wants to connect to Ruum</h1>
      <Loaded state={oauthClient}>
        {(c) => {
          if (!c) {
            return (
              <Note tone="warning" role="note">
                Ruum can't show the name of this app. Continue only if you started this.
              </Note>
            )
          }
          const from = hostOf(c.client_uri)
          const href = c.client_uri ? safeHref(c.client_uri) : undefined
          // The redirect of this request when the UI can read it; otherwise the registered ones.
          const back = requestRedirect ? [hostOf(requestRedirect)] : c.redirect_uris.map((u) => hostOf(u))
          const backText = back.map((h) => h ?? 'an address that is not valid').join(', ')
          return (
            <Stack>
              <Note tone="warning" role="note">
                Ruum can't check the name or address of this app. Continue only if you started this.
              </Note>
              <Text tone="muted">
                {from ? (
                  <>
                    It says it is from{' '}
                    {href ? (
                      <a href={href} rel="noreferrer noopener">
                        {from}
                      </a>
                    ) : (
                      from
                    )}
                    .{' '}
                  </>
                ) : null}
                {requestRedirect ? `It will go back to ${backText}.` : `It can go back to ${back.length > 1 ? 'one of ' : ''}${backText}.`}
              </Text>
            </Stack>
          )
        }}
      </Loaded>
      <p>
        It will act as an agent for you, <strong>{me.user.display_name}</strong>. It gets only the access you choose here, and never more
        than you have. You can pause it at any time on the Agents page.
      </p>
      <Stack as="form" onSubmit={submit}>
        <Field label="Space">
          <Select
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
          </Select>
        </Field>
        <Fieldset legend="Agent">
          <Radio name="mode" checked={mode === 'new'} onChange={() => setMode('new')} label="Create a new agent" />
          {mode === 'new' && (
            <Indent>
              <Field label="Agent name">
                <TextInput value={agentName} placeholder={appName} onChange={(e) => setAgentName(e.target.value)} />
              </Field>
              <ScopeFields tenantId={tenantId} spaceName={spaceName} scope={scope} onChange={setScope} idPrefix="consent" />
              {!folderMissing && <Note>{accessSummary(scope, null, spaceName)}.</Note>}
            </Indent>
          )}
          <Radio
            name="mode"
            checked={mode === 'existing'}
            disabled={!agents.data?.items.length}
            onChange={() => setMode('existing')}
            label="Use an existing agent"
          />
          {mode === 'existing' && agents.data && (
            <Indent>
              <Field label="Existing agent">
                <Select required value={agentId} onChange={(e) => setAgentId(e.target.value)}>
                  <option value="">Choose an agent</option>
                  {agents.data.items
                    .filter((a) => !a.disabled_at)
                    .map((a) => (
                      <option key={a.id} value={a.id}>
                        {a.name} ({roleLabels[a.scope.max_role].toLowerCase()})
                      </option>
                    ))}
                </Select>
              </Field>
            </Indent>
          )}
        </Fieldset>
        <ErrorNote error={error} />
        <Actions>
          <Button disabled={busy} onClick={() => answer(false)}>
            Don't allow
          </Button>
          <Button variant="primary" type="submit" disabled={busy || (mode === 'existing' && !agentId) || folderMissing}>
            Allow
          </Button>
        </Actions>
        {folderMissing && (
          <Text tone="muted" size="sm">
            Choose a folder first, or choose Everywhere.
          </Text>
        )}
      </Stack>
    </Page>
  )
}
