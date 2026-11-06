import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { db } from '../../mocks/db'
import { ids, personal, renderApp, team } from '../render'
import { recordRequests } from '../requests'

describe('app shell', () => {
  it('switches the tenant, keeps the section and sends the Ruum-Tenant header', async () => {
    const seen = recordRequests()
    const { user, router } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    expect(seen.find((r) => r.path === '/v1/nodes/root')?.headers.get('Ruum-Tenant')).toBe(ids.personal)

    await user.selectOptions(screen.getByRole('combobox', { name: 'Space' }), ids.team)
    expect(await screen.findByRole('button', { name: 'engine-specs.txt' })).toBeInTheDocument()
    expect(router.state.location.pathname).toBe(`${team}/files`)
    const teamRoot = seen.filter((r) => r.path === '/v1/nodes/root').at(-1)
    expect(teamRoot?.headers.get('Ruum-Tenant')).toBe(ids.team)
  })

  it('shows the space settings only to admins and owners', async () => {
    db.tenants[1].role = 'member'
    renderApp(`${team}/files`)
    const nav = await screen.findByRole('navigation', { name: 'Main' })
    expect(within(nav).queryByRole('link', { name: 'Space settings' })).not.toBeInTheDocument()
    expect(within(nav).getByRole('link', { name: 'Agents' })).toBeInTheDocument()
  })

  it('opens a space that is not in the list with only what was shared, and does not call the person a guest', async () => {
    renderApp('/t/0190a000-0000-7000-8000-0000000000ff/files')
    expect(await screen.findByRole('heading', { name: 'Shared with you in A shared space', level: 1 })).toBeInTheDocument()
    // The person is not in that space: "You're a guest here" would contradict the pages.
    expect(screen.queryByText(/You're a guest here/)).not.toBeInTheDocument()
    expect(screen.getByRole('combobox', { name: 'Space' })).toHaveDisplayValue('A shared space (shared with you)')
  })

  it('tells a guest of a space that they see only what was shared', async () => {
    db.tenants[1].role = 'guest'
    renderApp(`${team}/files`)
    expect(await screen.findByText("You're a guest here. You see only what people shared with you.")).toBeInTheDocument()
  })

  it('starts a team space from the space switcher', async () => {
    const { user, router } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.selectOptions(screen.getByRole('combobox', { name: 'Space' }), 'Start a team space...')
    const d = await screen.findByRole('dialog', { name: 'Start a team space' })
    expect(within(d).getByText("You'll be the owner. You can invite people next.")).toBeInTheDocument()
    await user.type(within(d).getByRole('textbox', { name: 'Team name' }), 'Pärn Studio{Enter}')
    expect(await screen.findByText('Pärn Studio is ready. Invite the people you work with.')).toBeInTheDocument()
    expect(router.state.location.pathname).toMatch(/\/admin\/invitations$/)
    // React Router commits the new location in a transition, after the toast: wait for it.
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Space' })).toHaveDisplayValue('Pärn Studio (owner)'))
  })

  it('says so when the browser is offline', async () => {
    renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    const online = vi.spyOn(navigator, 'onLine', 'get').mockReturnValue(false)
    window.dispatchEvent(new Event('offline'))
    expect(await screen.findByText("You're offline. Changes wait until you're back.")).toBeInTheDocument()
    online.mockReturnValue(true)
    window.dispatchEvent(new Event('online'))
    await waitFor(() => expect(screen.queryByText(/You're offline/)).not.toBeInTheDocument())
  })

  it('clears the search field when the person leaves the search page', async () => {
    const { user } = renderApp(`${personal}/search?q=plan`)
    expect(await screen.findByRole('searchbox', { name: 'Search by name' })).toHaveValue('plan')
    await user.click(screen.getByRole('link', { name: 'Trash' }))
    await screen.findByRole('heading', { name: 'Trash', level: 1 })
    expect(screen.getByRole('searchbox', { name: 'Search by name' })).toHaveValue('')
  })

  it('searches by name from the header', async () => {
    const { user, router } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.type(screen.getByRole('searchbox', { name: 'Search by name' }), 'plan{Enter}')
    expect(await screen.findByRole('link', { name: 'plan.txt' })).toBeInTheDocument()
    expect(screen.getByText('/Documents/plan.txt')).toBeInTheDocument()
    expect(router.state.location.search).toBe('?q=plan')
  })
})
