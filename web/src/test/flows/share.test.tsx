import { screen, waitFor, within } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { server } from '../../mocks/node'
import { choose, dialog, ids, personal, renderApp, team } from '../render'
import { recordRequests } from '../requests'

async function openShare(path: string, name: string) {
  const app = renderApp(path)
  await app.user.click(await screen.findByRole('button', { name: `Details of ${name}` }))
  const details = await screen.findByRole('region', { name })
  await app.user.click(within(details).getByRole('button', { name: 'Share' }))
  return { ...app, d: await dialog(`Share ${name}`) }
}

describe('share dialog', () => {
  it('shares with a person by email and role, notifies them, and says what Ruum emailed (0003-3)', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Charles Babbage')).toBeInTheDocument()
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'grace@example.com')
    await user.selectOptions(within(form).getByRole('combobox', { name: 'Access' }), 'comment')
    // The meaning of the role is under the menu.
    expect(within(d).getByText('Can comment: see and comment')).toBeInTheDocument()
    expect(within(form).getByText('They sign in with this address to open it.')).toBeInTheDocument()
    // "Notify people" is on by default, with an optional message; no invitation box any more.
    expect(within(form).getByRole('checkbox', { name: 'Notify people' })).toBeChecked()
    expect(within(form).queryByRole('checkbox', { name: /invitation/ })).not.toBeInTheDocument()
    await user.type(within(form).getByRole('textbox', { name: 'Message (optional)' }), 'The notes of Monday.')
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Shared with grace@example.com. Ruum emailed them.')).toBeInTheDocument()
    expect(await within(d).findByText('grace@example.com')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.notes}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'email', email: 'grace@example.com' }, role: 'comment', notify: true, message: 'The notes of Monday.' })
    // The share dialog never creates an invitation (decision 0003-3).
    expect(seen.some((r) => r.path.endsWith('/invitations'))).toBe(false)

    // A second share to the same person within 10 minutes sends no mail, and the dialog says so.
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'grace@example.com')
    await user.selectOptions(within(form).getByRole('combobox', { name: 'Access' }), 'view')
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(
      await within(d).findByText("Shared with grace@example.com. Ruum didn't email them this time. They'll find it under Shared with me."),
    ).toBeInTheDocument()
  })

  it('shares with a person outside the space in plain words, without mail, and offers a link that needs no account', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    await within(d).findByText('Charles Babbage')
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'cara@example.com')
    expect(
      within(form).getByText(
        "cara@example.com isn't in Ada Lovelace. They can open notes.md after they sign in with this address, and they join Ada Lovelace as a guest.",
      ),
    ).toBeInTheDocument()
    // Never "make an account".
    expect(within(form).queryByText(/make a free/)).not.toBeInTheDocument()
    // The way without an account is offered, and leads to the link form.
    await user.click(within(form).getByRole('button', { name: 'Use a link instead' }))
    expect(within(within(d).getByRole('form', { name: 'Create a link' })).getByRole('combobox', { name: 'Access' })).toHaveFocus()
    // Without "Notify people" there is no message field and no word about mail.
    await user.click(within(form).getByRole('checkbox', { name: 'Notify people' }))
    expect(within(form).queryByRole('textbox', { name: 'Message (optional)' })).not.toBeInTheDocument()
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Shared with cara@example.com.')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.notes}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'email', email: 'cara@example.com' }, role: 'view', notify: false })
    expect(seen.some((r) => r.path.endsWith('/invitations'))).toBe(false)
    // The new row is an email grant: invited, not joined yet.
    const row = (await within(d).findByText('cara@example.com', { selector: 'li *, li' })).closest('li')!
    expect(row).toHaveTextContent('Invited, not joined yet')
  })

  it('says when sharing is too fast (429)', async () => {
    server.use(
      http.post('*/v1/nodes/:id/grants', () =>
        HttpResponse.json({ error: { code: 'rate_limited', message: 'Too many grants.' } }, { status: 429 }),
      ),
    )
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'cara@example.com')
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(
      await within(d).findByText('You shared with many people in a short time. Wait a few minutes, then try again.'),
    ).toBeInTheDocument()
  })

  it('shares with a person of the space by user ID, so it reaches their account at once', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'Grace@Example.com')
    expect(within(form).queryByRole('checkbox', { name: /invitation/ })).not.toBeInTheDocument()
    // Grace is a member who can edit: a share can't give her less than that, and the form says so.
    expect(within(form).getByRole('note')).toHaveTextContent(
      'Grace Hopper already has "Can edit" everywhere in Analytical Engines from their role there. A share can\'t give them less. To let them see only some folders, make them a guest under Members.',
    )
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(
      await within(d).findByText(
        'Shared engine-specs.txt with Grace Hopper. They already have "Can edit" in Analytical Engines from their role there. Ruum emailed them.',
      ),
    ).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.teamSpecs}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'user', id: ids.grace }, role: 'view', notify: true })
    expect(seen.some((r) => r.path.endsWith('/invitations'))).toBe(false)
    // The row names her, never "Invited, not joined yet".
    const people = within(d).getByRole('region', { name: 'People with access' })
    const row = (await within(people).findByText('Grace Hopper')).closest('li')!
    expect(row).not.toHaveTextContent('Invited, not joined yet')
    expect(row).toHaveTextContent('Has "Can edit" everywhere in Analytical Engines from their role there; this share gives them nothing more.')
  })

  it('shows the access that comes from the space, by content role, and the caller\'s own (0003-1)', async () => {
    const { d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const people = within(d).getByRole('region', { name: 'People with access' })
    const admins = (await within(people).findByText('2 owners and admins of Analytical Engines')).closest('li')!
    expect(admins).toHaveTextContent('Can manage')
    const members = within(people).getByText('1 member of Analytical Engines').closest('li')!
    expect(members).toHaveTextContent('Can edit')
    const mine = within(people).getByText('You, from your role in the space').closest('li')!
    expect(mine).toHaveTextContent('As admin of Analytical Engines. A share here can\'t give you less.')
    expect(mine).toHaveTextContent('Can manage')
    expect(within(people).queryByText('Only you can see this.')).not.toBeInTheDocument()
  })

  it('shows one row for the space while everyone in it manages', async () => {
    db.members.find((m) => m.user_id === ids.grace && m.tenant_id === ids.team)!.content_role = 'manage'
    const { d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const people = within(d).getByRole('region', { name: 'People with access' })
    const row = (await within(people).findByText(/Everyone in Analytical Engines except guests/)).closest('li')!
    expect(row).toHaveTextContent('Can manage')
    await waitFor(() => expect(row).toHaveTextContent('3 people. Owners, admins and members manage everything in Analytical Engines, and share it.'))
  })

  it('shares with everyone in the space, and says that view members get the role too (0003-1)', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.click(within(form).getByRole('button', { name: 'Share with everyone in Analytical Engines instead' }))
    await user.selectOptions(within(form).getByRole('combobox', { name: 'Access' }), 'edit')
    expect(within(form).getByRole('note')).toHaveTextContent('Everyone in Analytical Engines gets Can edit, also members who can only view.')
    expect(within(form).queryByRole('checkbox', { name: 'Notify people' })).not.toBeInTheDocument()
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Shared with Everyone in Analytical Engines.')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.teamSpecs}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'tenant_members' }, role: 'edit', notify: false })
  })

  it('does not let a person change their own row, and says who added a grant', async () => {
    db.grants.push({
      id: '0190a000-0000-7000-8000-0000000007f1',
      node_id: ids.notes,
      grantee: { type: 'user', id: ids.ada },
      role: 'edit',
      inherited_from: null,
      created_by: { actor: { kind: 'user', id: ids.charles, display_name: 'Charles Babbage' }, on_behalf_of: null },
      created_at: '2026-11-01T10:00:00Z',
    })
    const { d } = await openShare(`${personal}/files`, 'notes.md')
    const people = within(d).getByRole('region', { name: 'People with access' })
    const mine = (await within(people).findByText(/\(you\)/)).closest('li')!
    expect(within(mine).queryByRole('combobox')).not.toBeInTheDocument()
    expect(within(mine).queryByRole('button', { name: /^Remove/ })).not.toBeInTheDocument()
    expect(mine).toHaveTextContent('added by Charles Babbage')
  })

  it('changes and removes access', async () => {
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    // The owner is the first row.
    const people = within(d).getByRole('region', { name: 'People with access' })
    expect(within(people).getAllByRole('listitem')[0]).toHaveTextContent(/Ada Lovelace.*Owner/)
    // The role menu of a row says what each role means, and ends with "Remove access" (design-system 4.8).
    await choose(user, /Access of Charles Babbage$/, /^Can view/, d)
    await waitFor(() => expect(db.grants.find((g) => g.node_id === ids.notes)?.role).toBe('view'))
    await choose(user, /Access of Charles Babbage$/, 'Remove access', d)
    expect(await within(d).findByText('Only you can see this.')).toBeInTheDocument()
  })

  it('creates a share link and shows its URL once', async () => {
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Anyone with the link: off')).toBeInTheDocument()
    // The policy of the personal space has no edit links.
    const form = within(d).getByRole('form', { name: 'Create a link' })
    const role = within(form).getByRole('combobox', { name: 'Access' })
    expect(within(role).queryByRole('option', { name: 'Can edit' })).not.toBeInTheDocument()
    // The default end is visible: 7 days.
    expect(within(form).getByRole('spinbutton', { name: 'Ends after (days)' })).toHaveValue(7)
    await user.click(within(d).getByRole('button', { name: 'Create link' }))
    const url = await within(d).findByRole('textbox', { name: 'New link' })
    expect((url as HTMLInputElement).value).toMatch(/\/s#[0-9A-Za-z_-]+$/)
    expect(await within(d).findByText('Anyone with the link: can view')).toBeInTheDocument()
  })

  it('revokes a share link after a confirmation', async () => {
    const { user } = await openShare(`${personal}/files/${ids.documents}`, 'report.txt')
    await user.click(await screen.findByRole('button', { name: 'Turn off the can view link' }))
    const confirm = await dialog('Turn off this link?')
    await user.click(within(confirm).getByRole('button', { name: 'Turn off link' }))
    const d = await dialog('Share report.txt')
    expect(await within(d).findByText('Anyone with the link: off')).toBeInTheDocument()
  })

  it('does not list revoked links', async () => {
    db.shareLinks[0].revoked_at = '2026-11-02T00:00:00Z'
    const { d } = await openShare(`${personal}/files/${ids.documents}`, 'report.txt')
    expect(await within(d).findByText('Anyone with the link: off')).toBeInTheDocument()
    expect(within(d).queryByRole('button', { name: 'Turn off the can view link' })).not.toBeInTheDocument()
  })

  it('offers only roles up to the caller\'s own role', async () => {
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    notes.my_role = 'edit'
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    const add = within(within(d).getByRole('form', { name: 'Add people' })).getByRole('combobox', { name: 'Access' })
    expect(within(add).getAllByRole('option').map((o) => o.textContent)).toEqual(['Can view', 'Can comment', 'Can edit'])
    await user.click(await within(d).findByRole('button', { name: /Access of Charles Babbage$/ }))
    const menu = screen.getByRole('menu')
    expect(within(menu).getAllByRole('menuitemradio').map((o) => o.firstChild?.nextSibling?.firstChild?.textContent)).toEqual([
      'Can view',
      'Can comment',
      'Can edit',
    ])
  })

  it('shows a grant above the caller\'s role without controls', async () => {
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    notes.my_role = 'comment'
    const { d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Charles Babbage')).toBeInTheDocument()
    expect(within(d).queryByRole('button', { name: /Access of Charles Babbage$/ })).not.toBeInTheDocument()
    const link = within(within(d).getByRole('form', { name: 'Create a link' })).getByRole('combobox', { name: 'Access' })
    expect(within(link).getAllByRole('option').map((o) => o.textContent)).toEqual(['Can view', 'Can comment'])
  })

  it('sets "editors can share" back to inherit', async () => {
    const seen = recordRequests()
    db.nodes.find((n) => n.id === ids.documents)!.editors_can_share = false
    const { user, d } = await openShare(`${personal}/files`, 'Documents')
    const setting = within(d).getByRole('combobox', { name: /People who can edit can also share/ })
    expect(setting).toHaveValue('false')
    // The inherit choice shows the value above this folder (the top: yes), not the folder's own "no".
    expect(await within(setting).findByRole('option', { name: 'Standard (now: yes)' })).toBeInTheDocument()
    await user.selectOptions(setting, 'inherit')
    await waitFor(() => expect(seen.find((r) => r.method === 'PATCH' && r.path === `/v1/nodes/${ids.documents}/sharing`)?.body).toEqual({ editors_can_share: null }))
    // The save is confirmed next to the rule.
    expect(await within(within(d).getByRole('region', { name: 'Sharing rule' })).findByText('Saved.')).toBeInTheDocument()
  })

  it('shows inherited access without controls', async () => {
    const { d } = await openShare(`${personal}/files/${ids.documents}`, 'report.txt')
    expect(await within(d).findByText('grace@example.com')).toBeInTheDocument()
    // The row names the folder the access comes from, with a link to it.
    expect(await within(d).findByRole('link', { name: 'Documents' })).toBeInTheDocument()
    expect(within(d).getByText(/from folder/)).toBeInTheDocument()
    expect(within(d).queryByRole('combobox', { name: 'Access of grace@example.com' })).not.toBeInTheDocument()
  })
})
