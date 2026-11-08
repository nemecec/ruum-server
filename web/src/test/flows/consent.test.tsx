import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { clientIdOfRequest, redirectUriOfRequest } from '../../app/oauth'
import { db } from '../../mocks/db'
import { ids, renderApp } from '../render'
import { recordRequests } from '../requests'

const b64 = (o: object) => btoa(JSON.stringify(o)).replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_')
// The shape the server seals (ruum-core identity::tokens::Sealed): the claims are inside `v`.
const sealed = (v: object) =>
  `${b64({ alg: 'ES256', typ: 'JWT' })}.${b64({ iss: 'http://localhost', aud: 'ruum:oauth_request', iat: 1, exp: 2, v })}.sig`
const request = sealed({ client_id: ids.oauthClient, redirect_uri: 'https://claude.example/callback', code_challenge: 'x', resource: 'http://localhost' })
const bare = sealed({ client_id: ids.oauthClient })

describe('OAuth consent screen', () => {
  it('reads the client from the request value', () => {
    expect(clientIdOfRequest(request)).toBe(ids.oauthClient)
    expect(redirectUriOfRequest(request)).toBe('https://claude.example/callback')
    // The top-level shape still works.
    expect(clientIdOfRequest(`${b64({ alg: 'ES256' })}.${b64({ client_id: 'top' })}.sig`)).toBe('top')
    expect(clientIdOfRequest('opaque')).toBeNull()
  })

  it('approves with a new agent and follows the redirect', async () => {
    const seen = recordRequests()
    const { user, assign } = renderApp(`/oauth/consent?request=${request}`)
    expect(await screen.findByRole('heading', { name: 'Claude Desktop wants to connect to Ruum' })).toBeInTheDocument()
    await user.selectOptions(screen.getByRole('combobox', { name: 'Space' }), ids.team)
    await user.selectOptions(screen.getByRole('combobox', { name: 'Highest role' }), 'view')
    // The default is a folder the person chooses: without one, Allow waits.
    expect(screen.getByRole('button', { name: 'Allow' })).toBeDisabled()
    await user.click(screen.getByRole('radio', { name: /^Everywhere you have access in Analytical Engines/ }))
    expect(screen.getByText(/^Can view everywhere you have access in Analytical Engines, until /)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Allow' }))
    await waitFor(() => expect(assign).toHaveBeenCalledWith('https://claude.example/callback?code=mock-code'))
    expect(await screen.findByText('You can go back to Claude Desktop.')).toBeInTheDocument()
    const post = seen.find((r) => r.path === '/v1/oauth/consent')
    // Access ends in 90 days by default.
    const body = post?.body as { new_agent: { scope: { expires_at: string } } }
    const days = (new Date(body.new_agent.scope.expires_at).getTime() - Date.now()) / 86400_000
    expect(Math.round(days)).toBe(90)
    expect(post?.body).toEqual({
      request,
      approve: true,
      tenant_id: ids.team,
      new_agent: { name: 'Claude Desktop', scope: { subtree: null, max_role: 'view', expires_at: body.new_agent.scope.expires_at } },
    })
    expect(db.agents.some((a) => a.tenant_id === ids.team && a.name === 'Claude Desktop')).toBe(true)
  })

  it('says that the name of the application is self-declared', async () => {
    renderApp(`/oauth/consent?request=${bare}`)
    expect(await screen.findByRole('note')).toHaveTextContent("Ruum can't check the name or address of this app.")
    expect(screen.getByText(/It can go back to claude\.example\./)).toBeInTheDocument()
  })

  it('shows only the redirect of this request when the request carries it', async () => {
    db.oauthClients[0].redirect_uris.push('https://other.example/cb')
    const withRedirect = sealed({ client_id: ids.oauthClient, redirect_uri: 'https://claude.example/callback' })
    renderApp(`/oauth/consent?request=${withRedirect}`)
    expect(await screen.findByText(/It will go back to claude\.example\./)).toBeInTheDocument()
    expect(screen.queryByText(/other\.example/)).not.toBeInTheDocument()
  })

  it('does not break on malformed client URLs', async () => {
    db.oauthClients[0].client_uri = 'not a url'
    db.oauthClients[0].redirect_uris = ['::bad::']
    renderApp(`/oauth/consent?request=${bare}`)
    expect(await screen.findByText(/an address that isn't valid/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Allow' })).toBeInTheDocument()
  })

  it('gives a new agent a narrow scope: one folder and an end date', async () => {
    const seen = recordRequests()
    const { user, assign } = renderApp(`/oauth/consent?request=${request}`)
    await screen.findByRole('heading', { name: 'Claude Desktop wants to connect to Ruum' })
    await user.click(screen.getByRole('button', { name: 'Choose folder' }))
    const picker = await screen.findByRole('dialog', { name: 'Choose a folder for the agent' })
    await user.click(await within(picker).findByRole('button', { name: 'Documents' }))
    await user.click(await within(picker).findByRole('button', { name: 'Use this folder' }))
    expect(await screen.findByText('Folder Documents')).toBeInTheDocument()
    await user.clear(screen.getByLabelText('Ends on (optional)'))
    await user.type(screen.getByLabelText('Ends on (optional)'), '2026-12-31')
    await user.click(screen.getByRole('button', { name: 'Allow' }))
    await waitFor(() => expect(assign).toHaveBeenCalled())
    const body = seen.find((r) => r.path === '/v1/oauth/consent')?.body as { new_agent: { scope: unknown } }
    expect(body.new_agent.scope).toEqual({ subtree: ids.documents, max_role: 'edit', expires_at: '2026-12-31T23:59:59.000Z' })
    // The folder calls name the chosen tenant explicitly; the consent call itself has no tenant header.
    expect(seen.filter((r) => r.path.startsWith('/v1/nodes')).every((r) => r.headers.get('Ruum-Tenant') === ids.personal)).toBe(true)
    expect(seen.find((r) => r.path === '/v1/oauth/consent')?.headers.has('Ruum-Tenant')).toBe(false)
  })

  it('binds to an existing agent', async () => {
    const seen = recordRequests()
    const { user, assign } = renderApp(`/oauth/consent?request=${request}`)
    await user.click(await screen.findByRole('radio', { name: 'Use an existing agent' }))
    await user.selectOptions(await screen.findByRole('combobox', { name: 'Existing agent' }), ids.agentClaude)
    await user.click(screen.getByRole('button', { name: 'Allow' }))
    await waitFor(() => expect(assign).toHaveBeenCalled())
    expect(seen.find((r) => r.path === '/v1/oauth/consent')?.body).toEqual({
      request,
      approve: true,
      tenant_id: ids.personal,
      agent_id: ids.agentClaude,
    })
  })

  it('denies', async () => {
    const { user, assign } = renderApp(`/oauth/consent?request=${request}`)
    await user.click(await screen.findByRole('button', { name: "Don't allow" }))
    await waitFor(() => expect(assign).toHaveBeenCalledWith('https://claude.example/callback?error=access_denied'))
  })

  it('sends a visitor without a session through sign-in and back', async () => {
    db.signedIn = false
    const { router } = renderApp(`/oauth/consent?request=${request}`)
    await screen.findByRole('heading', { name: 'Sign in to Ruum' })
    const link = screen.getByRole('link', { name: 'Continue with Google' })
    const returnTo = new URL(link.getAttribute('href')!, 'http://x').searchParams.get('return_to')
    expect(returnTo).toBe(`/oauth/consent?request=${request}`)
    expect(router.state.location.pathname).toBe('/signin')
  })
})
