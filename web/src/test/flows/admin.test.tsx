import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { choose, dialog, ids, renderApp, team } from '../render'
import { recordRequests } from '../requests'

describe('tenant admin', () => {
  it('opens on the members page and changes a role', async () => {
    const { user, router } = renderApp(`${team}/admin`)
    expect(await screen.findByRole('heading', { name: 'Members' })).toBeInTheDocument()
    expect(router.state.location.pathname).toBe(`${team}/admin/members`)
    // An admin cannot change the owner or themselves, and the page says why (journey J6).
    const owner = (await screen.findByRole('rowheader', { name: /^Charles Babbage/ })).closest('tr')!
    expect(within(owner).queryByRole('button', { name: /Role of/ })).not.toBeInTheDocument()
    expect(within(owner).getByText('Only an owner can change the role of an owner.')).toBeInTheDocument()
    const self = screen.getByRole('rowheader', { name: /^Ada Lovelace/ }).closest('tr')!
    expect(within(self).getByText("You can't change your own role. Another admin or owner can.")).toBeInTheDocument()
    // The role menu says what each role means.
    await user.click(screen.getByRole('button', { name: 'Member Role of Grace Hopper' }))
    expect(within(screen.getByRole('menu')).getByRole('menuitemradio', { name: /^Admin/ })).toHaveTextContent(/Admin.+/)
    await user.click(within(screen.getByRole('menu')).getByRole('menuitemradio', { name: /^Admin/ }))
    expect(await screen.findByText('Grace Hopper is now admin.')).toBeInTheDocument()
    expect(db.members.find((m) => m.user_id === ids.grace && m.tenant_id === ids.team)?.role).toBe('admin')
  })

  it('asks before a member becomes a guest, and says what they lose', async () => {
    const { user } = renderApp(`${team}/admin/members`)
    await choose(user, 'Member Role of Grace Hopper', /^Guest/)
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
    await choose(user, 'More actions for Grace Hopper', 'Remove from Analytical Engines')
    const d = await dialog('Remove Grace Hopper from Analytical Engines?')
    expect(within(d).getByText(/Their files stay\./)).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Remove Grace Hopper' }))
    await waitFor(() => expect(screen.queryByRole('rowheader', { name: /^Grace Hopper/ })).not.toBeInTheDocument())
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
    expect(await screen.findByText('2 invitations are on their way.')).toBeInTheDocument()
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

  it('gives a member a content role, and a guest who becomes a member one in the same step (0003-1)', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${team}/admin/members`)
    // The content role menu says what each one means.
    await user.click(await screen.findByRole('button', { name: 'Can edit what Grace Hopper can do' }))
    const menu = screen.getByRole('menu')
    expect(within(menu).getByRole('menuitemradio', { name: /^Can view/ })).toHaveTextContent(
      'Sees everything in Analytical Engines, changes nothing. To show only some folders, make them a guest.',
    )
    await user.click(within(menu).getByRole('menuitemradio', { name: /^Can view/ }))
    expect(await screen.findByText('Grace Hopper: can view in Analytical Engines now.')).toBeInTheDocument()
    expect(seen.find((r) => r.method === 'PATCH' && r.path.endsWith(`/members/${ids.grace}`))?.body).toEqual({ role: 'member', content_role: 'view' })
    expect(await screen.findByText('Shares to everyone in the space still apply.')).toBeInTheDocument()

    // Guest to member asks for the content role (default Can manage).
    await choose(user, /Role of Grace Hopper/, /^Guest/)
    await user.click(within(await dialog('Make Grace Hopper a guest?')).getByRole('button', { name: 'Make guest' }))
    await screen.findByText('Grace Hopper is now guest.')
    await choose(user, 'Guest Role of Grace Hopper', /^Member/)
    const d = await dialog('Make Grace Hopper a member?')
    expect(within(d).getByRole('radio', { name: 'Can manage' })).toBeChecked()
    await user.click(within(d).getByRole('radio', { name: 'Can comment' }))
    await user.click(within(d).getByRole('button', { name: 'Make member' }))
    await screen.findByText('Grace Hopper is now member.')
    expect(seen.filter((r) => r.method === 'PATCH').at(-1)?.body).toEqual({ role: 'member', content_role: 'comment' })
  })

  it('invites a member with a content role, and leaves it out for other roles (0003-1)', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${team}/admin/invitations`)
    const form = await screen.findByRole('form', { name: 'Invite people' })
    const what = within(form).getByRole('combobox', { name: 'What they can do' })
    expect(what).toHaveValue('manage')
    await user.selectOptions(what, 'edit')
    expect(within(form).getByText(/Can edit: Changes files and folders in Analytical Engines/)).toBeInTheDocument()
    await user.type(within(form).getByRole('textbox', { name: 'Emails' }), 'kathleen@example.com')
    await user.click(within(form).getByRole('button', { name: 'Send invitations' }))
    await screen.findByText(/Invitation on its way|kathleen@example.com/)
    await user.selectOptions(within(form).getByRole('combobox', { name: 'Role' }), 'admin')
    expect(within(form).queryByRole('combobox', { name: 'What they can do' })).not.toBeInTheDocument()
    await user.type(within(form).getByRole('textbox', { name: 'Emails' }), 'alan.t@example.com')
    await user.click(within(form).getByRole('button', { name: 'Send invitations' }))
    await waitFor(() => expect(seen.filter((r) => r.method === 'POST' && r.path.endsWith('/invitations'))).toHaveLength(2))
    expect(seen.filter((r) => r.method === 'POST' && r.path.endsWith('/invitations')).map((r) => r.body)).toEqual([
      { email: 'kathleen@example.com', role: 'member', content_role: 'edit' },
      { email: 'alan.t@example.com', role: 'admin' },
    ])
  })

  it('shows sign-ins in the activity log, with where they came from (0003-4)', async () => {
    const { user } = renderApp(`${team}/admin/audit`)
    await user.selectOptions(await screen.findByRole('combobox', { name: 'Show' }), 'auth.')
    expect(await screen.findByText("Sign-ins of members. Guests' sign-ins aren't shown here.")).toBeInTheDocument()
    const login = (await screen.findByText('signed in')).closest('li')!
    expect(login).toHaveTextContent('192.0.2.10 · Firefox on macOS')
    expect(screen.getByText('opened Analytical Engines (new session)')).toBeInTheDocument()
    expect(screen.getByText('signed out')).toBeInTheDocument()
    await user.selectOptions(screen.getByRole('combobox', { name: 'Show' }), 'agent.')
    const refused = (await screen.findByText('was refused (key revoked)')).closest('li')!
    expect(within(refused).getByText('Refused')).toBeInTheDocument()
    expect(screen.getByText('got a token')).toBeInTheDocument()
  })

  it('says how a person joined through a share to their email (0003-2)', async () => {
    renderApp(`${team}/admin/audit`)
    expect(await screen.findByText(/joined as a guest through a share by Ada Lovelace/)).toBeInTheDocument()
    expect(screen.getByText(/\(view; may join as a guest\)/)).toBeInTheDocument()
  })

  it('shows the plan, limits and usage', async () => {
    renderApp(`${team}/admin/limits`)
    expect(await screen.findByText('Team')).toBeInTheDocument()
    expect(screen.getByRole('meter', { name: 'Agents' })).toHaveAttribute('value', '7')
    expect(screen.getByText('3 of no limit')).toBeInTheDocument()
  })

  it('lists and filters the activity log in plain words', async () => {
    const { user } = renderApp(`${team}/admin/audit`)
    expect(await screen.findByText(/invited alan@example\.com/)).toBeInTheDocument()
    expect(screen.getByText(/created an agent: Claude Desktop/)).toBeInTheDocument()
    expect(screen.queryByText('agent.registered')).not.toBeInTheDocument()
    // The filter applies on change.
    await user.selectOptions(screen.getByRole('combobox', { name: 'Show' }), 'agent.')
    await waitFor(() => expect(screen.queryByText(/invited alan/)).not.toBeInTheDocument())
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
    renderApp(`${team}/admin/audit`)
    expect(await screen.findByText('Only admins and owners of Analytical Engines can open these pages.')).toBeInTheDocument()
  })

  it('keeps guests out of the members page', async () => {
    db.tenants[1].role = 'guest'
    renderApp(`${team}/admin/members`)
    expect(await screen.findByText('Only admins and owners of Analytical Engines can open these pages.')).toBeInTheDocument()
  })

  it('shows members the people of the space read-only, with their content roles (0003-1)', async () => {
    db.tenants[1].role = 'member'
    renderApp(`${team}/admin/members`)
    expect(await screen.findByText('The people of Analytical Engines. Only admins can change roles.')).toBeInTheDocument()
    const row = (await screen.findByRole('rowheader', { name: /^Grace Hopper/ })).closest('tr')!
    expect(within(row).getByText('Can edit')).toBeInTheDocument()
    expect(within(row).queryByRole('button')).not.toBeInTheDocument()
    expect(screen.queryByRole('navigation', { name: 'Space settings' })).not.toBeInTheDocument()
  })
})
