import { screen, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { ids, personal, renderApp, team } from '../render'
import { recordRequests } from '../requests'

describe('app shell', () => {
  it('switches the tenant, keeps the section and sends the Ruum-Tenant header', async () => {
    const seen = recordRequests()
    const { user, router } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    expect(seen.find((r) => r.path === '/v1/nodes/root')?.headers.get('Ruum-Tenant')).toBe(ids.personal)

    await user.selectOptions(screen.getByRole('combobox', { name: 'Tenant' }), ids.team)
    expect(await screen.findByRole('button', { name: 'engine-specs.txt' })).toBeInTheDocument()
    expect(router.state.location.pathname).toBe(`${team}/files`)
    const teamRoot = seen.filter((r) => r.path === '/v1/nodes/root').at(-1)
    expect(teamRoot?.headers.get('Ruum-Tenant')).toBe(ids.team)
  })

  it('shows the admin link only to admins and owners', async () => {
    db.tenants[1].role = 'member'
    renderApp(`${team}/files`)
    const nav = await screen.findByRole('navigation', { name: 'Main' })
    expect(within(nav).queryByRole('link', { name: 'Admin' })).not.toBeInTheDocument()
    expect(within(nav).getByRole('link', { name: 'Agents' })).toBeInTheDocument()
  })

  it('says so for a tenant the user is not a member of', async () => {
    renderApp('/t/0190a000-0000-7000-8000-0000000000ff/files')
    expect(await screen.findByRole('heading', { name: 'Tenant not found' })).toBeInTheDocument()
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
