import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, renderApp, team } from '../render'
import { recordRequests } from '../requests'

describe('tenant admin', () => {
  it('opens on the members page and changes a role', async () => {
    const { user, router } = renderApp(`${team}/admin`)
    expect(await screen.findByRole('heading', { name: 'Members' })).toBeInTheDocument()
    expect(router.state.location.pathname).toBe(`${team}/admin/members`)
    // An admin cannot change the owner or themselves.
    expect(await screen.findByRole('combobox', { name: 'Role of Charles Babbage' })).toBeDisabled()
    expect(screen.getByRole('combobox', { name: 'Role of Ada Lovelace' })).toBeDisabled()
    await user.selectOptions(screen.getByRole('combobox', { name: 'Role of Grace Hopper' }), 'admin')
    expect(await screen.findByText('Grace Hopper is now admin.')).toBeInTheDocument()
    expect(db.members.find((m) => m.user_id === ids.grace && m.tenant_id === ids.team)?.role).toBe('admin')
  })

  it('asks before a member becomes a guest, and says what they lose', async () => {
    const { user } = renderApp(`${team}/admin/members`)
    await user.selectOptions(await screen.findByRole('combobox', { name: 'Role of Grace Hopper' }), 'guest')
    const d = await dialog('Make Grace Hopper a guest?')
    expect(d).toHaveTextContent('Grace Hopper then sees only the items that are shared with them directly, and loses access to everything else in Analytical Engines at once.')
    // Nothing changed before the confirmation.
    expect(db.members.find((m) => m.user_id === ids.grace && m.tenant_id === ids.team)?.role).toBe('member')
    await user.click(within(d).getByRole('button', { name: 'Make guest' }))
    expect(await screen.findByText('Grace Hopper is now guest.')).toBeInTheDocument()
    expect(db.members.find((m) => m.user_id === ids.grace && m.tenant_id === ids.team)?.role).toBe('guest')
  })

  it('removes a member after a confirmation', async () => {
    const { user } = renderApp(`${team}/admin/members`)
    await user.click(await screen.findByRole('button', { name: 'Remove Grace Hopper' }))
    const d = await dialog('Remove Grace Hopper from Analytical Engines?')
    expect(within(d).getByText(/Their files stay\./)).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Remove Grace Hopper' }))
    await waitFor(() => expect(screen.queryByRole('rowheader', { name: 'Grace Hopper' })).not.toBeInTheDocument())
  })

  it('invites someone and revokes an invitation', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${team}/admin/invitations`)
    expect(await screen.findByText('alan@example.com')).toBeInTheDocument()
    const form = screen.getByRole('form', { name: 'Invite people' })
    // A team space offers members; the role's meaning is under the menu.
    expect(within(form).getByText('Member: can see, change, delete and share everything in the team space')).toBeInTheDocument()
    await user.type(within(form).getByRole('textbox', { name: 'Emails' }), 'kathleen@example.com, alan.t@example.com')
    await user.selectOptions(within(form).getByRole('combobox', { name: 'Role' }), 'guest')
    await user.click(within(form).getByRole('button', { name: 'Send invitations' }))
    expect(await screen.findByText('2 invitations sent.')).toBeInTheDocument()
    expect(seen.filter((r) => r.method === 'POST' && r.path.endsWith('/invitations')).map((r) => r.body)).toEqual([
      { email: 'kathleen@example.com', role: 'guest' },
      { email: 'alan.t@example.com', role: 'guest' },
    ])

    await user.click(screen.getByRole('button', { name: 'Cancel invitation: alan@example.com' }))
    await user.click(within(await dialog('Cancel this invitation?')).getByRole('button', { name: 'Cancel invitation' }))
    await waitFor(() => expect(screen.queryByText('alan@example.com')).not.toBeInTheDocument())
  })

  it('offers only guests in a personal space, and says why', async () => {
    const { user } = renderApp(`/t/${ids.personal}/admin/invitations`)
    const form = await screen.findByRole('form', { name: 'Invite people' })
    const role = within(form).getByRole('combobox', { name: 'Role' })
    await waitFor(() => expect(within(role).getAllByRole('option').map((o) => o.textContent)).toEqual(['Guest']))
    expect(within(form).getByText(/is a personal space, just for you/)).toBeInTheDocument()
    await user.click(within(form).getByRole('button', { name: 'Start a team space' }))
    expect(await dialog('Start a team space')).toBeInTheDocument()
  })

  it('saves the policy', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${team}/admin/policy`)
    await user.click(await screen.findByRole('checkbox', { name: 'Allow links' }))
    expect(screen.getByRole('checkbox', { name: 'Allow links that can edit' })).toBeDisabled()
    await user.click(screen.getByRole('button', { name: 'Save' }))
    expect(await screen.findByText('Saved the sharing rules.')).toBeInTheDocument()
    const patch = seen.find((r) => r.method === 'PATCH' && r.path === `/v1/tenants/${ids.team}`)
    expect(patch?.body).toMatchObject({ name: 'Analytical Engines', policy: { allow_share_links: false } })
  })

  it('shows the plan, limits and usage', async () => {
    renderApp(`${team}/admin/limits`)
    expect(await screen.findByText('Team')).toBeInTheDocument()
    expect(screen.getByRole('meter', { name: 'Agents' })).toHaveAttribute('value', '7')
    expect(screen.getByText('3 of no limit')).toBeInTheDocument()
  })

  it('lists and filters the activity log in plain words', async () => {
    const { user } = renderApp(`${team}/admin/audit`)
    expect(await screen.findByText(/invited someone: alan@example\.com/)).toBeInTheDocument()
    expect(screen.getByText(/created an agent: Claude Desktop/)).toBeInTheDocument()
    expect(screen.queryByText('agent.registered')).not.toBeInTheDocument()
    await user.selectOptions(screen.getByRole('combobox', { name: 'Show' }), 'agent.')
    await user.click(screen.getByRole('button', { name: 'Show' }))
    await waitFor(() => expect(screen.queryByText(/invited someone/)).not.toBeInTheDocument())
    expect(screen.getByText(/created an agent: Claude Desktop/)).toBeInTheDocument()
  })

  it('says who gave whom access to what, and the old and new values', async () => {
    renderApp(`${team}/admin/audit`)
    const access = (await screen.findByText(/gave Grace Hopper edit access to/)).closest('li')!
    // The item is a link to it.
    expect(await within(access).findByRole('link', { name: 'engine-specs.txt' })).toHaveAttribute('href', `${team}/item/${ids.teamSpecs}`)
    expect(access).toHaveTextContent('Ada Lovelace gave Grace Hopper edit access to engine-specs.txt')
    expect(screen.getByText(/changed Grace Hopper from Guest to Member/)).toBeInTheDocument()
    expect(
      await screen.findByText(/changed the sharing rule of/, { selector: 'span' }),
    ).toBeInTheDocument()
    expect(screen.getByText(/: people who can edit can also share, from same as the folder above to no/)).toBeInTheDocument()
  })

  it('keeps non-admins out', async () => {
    db.tenants[1].role = 'member'
    renderApp(`${team}/admin/members`)
    expect(await screen.findByText('Only admins and owners of Analytical Engines can open these pages.')).toBeInTheDocument()
  })
})
