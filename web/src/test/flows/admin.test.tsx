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

  it('removes a member after a confirmation', async () => {
    const { user } = renderApp(`${team}/admin/members`)
    await user.click(await screen.findByRole('button', { name: 'Remove Grace Hopper' }))
    await user.click(within(await dialog('Remove Grace Hopper?')).getByRole('button', { name: 'Remove member' }))
    await waitFor(() => expect(screen.queryByRole('rowheader', { name: 'Grace Hopper' })).not.toBeInTheDocument())
  })

  it('invites someone and revokes an invitation', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${team}/admin/invitations`)
    expect(await screen.findByText('alan@example.com')).toBeInTheDocument()
    const form = screen.getByRole('form', { name: 'Invite someone' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'kathleen@example.com')
    await user.selectOptions(within(form).getByRole('combobox', { name: 'Role' }), 'guest')
    await user.click(within(form).getByRole('button', { name: 'Send invitation' }))
    expect(await screen.findByText('Invitation sent to kathleen@example.com.')).toBeInTheDocument()
    expect(seen.find((r) => r.method === 'POST' && r.path.endsWith('/invitations'))?.body).toEqual({
      email: 'kathleen@example.com',
      role: 'guest',
    })

    await user.click(screen.getByRole('button', { name: 'Revoke invitation of alan@example.com' }))
    await user.click(within(await dialog('Revoke this invitation?')).getByRole('button', { name: 'Revoke' }))
    await waitFor(() => expect(screen.queryByText('alan@example.com')).not.toBeInTheDocument())
  })

  it('saves the policy', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${team}/admin/policy`)
    await user.click(await screen.findByRole('checkbox', { name: 'Allow share links' }))
    expect(screen.getByRole('checkbox', { name: 'Allow share links that can edit' })).toBeDisabled()
    await user.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(await screen.findByText('Policy saved.')).toBeInTheDocument()
    const patch = seen.find((r) => r.method === 'PATCH' && r.path === `/v1/tenants/${ids.team}`)
    expect(patch?.body).toMatchObject({ name: 'Analytical Engines', policy: { allow_share_links: false } })
  })

  it('shows the plan, limits and usage', async () => {
    renderApp(`${team}/admin/limits`)
    expect(await screen.findByText('Team')).toBeInTheDocument()
    expect(screen.getByRole('meter', { name: 'Agents' })).toHaveAttribute('value', '7')
    expect(screen.getByText('3 of no limit')).toBeInTheDocument()
  })

  it('lists and filters the audit log', async () => {
    const { user } = renderApp(`${team}/admin/audit`)
    expect(await screen.findByText('member.invited')).toBeInTheDocument()
    expect(screen.getByText('agent.registered')).toBeInTheDocument()
    await user.type(screen.getByRole('textbox', { name: 'Action starts with' }), 'agent.{Enter}')
    await waitFor(() => expect(screen.queryByText('member.invited')).not.toBeInTheDocument())
    expect(screen.getByText('agent.registered')).toBeInTheDocument()
  })

  it('keeps non-admins out', async () => {
    db.tenants[1].role = 'member'
    renderApp(`${team}/admin/members`)
    expect(await screen.findByText('Only admins and owners of Analytical Engines can open these pages.')).toBeInTheDocument()
  })
})
