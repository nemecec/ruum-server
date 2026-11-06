import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp, team } from '../render'
import { recordRequests } from '../requests'

async function openShare(path: string, name: string) {
  const app = renderApp(path)
  await app.user.click(await screen.findByRole('button', { name: `Details of ${name}` }))
  const details = await screen.findByRole('region', { name })
  await app.user.click(within(details).getByRole('button', { name: 'Share' }))
  return { ...app, d: await dialog(`Share ${name}`) }
}

describe('share dialog', () => {
  it('shares with a person by email and role, and says what Ruum emails', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Charles Babbage')).toBeInTheDocument()
    await user.type(within(d).getByRole('textbox', { name: 'Email' }), 'grace@example.com')
    await user.selectOptions(within(within(d).getByRole('form', { name: 'Add people' })).getByRole('combobox', { name: 'Access' }), 'comment')
    // The meaning of the role is under the menu.
    expect(within(d).getByText('Can comment: see and comment')).toBeInTheDocument()
    // No out-of-date "can't send emails" text: the one box says what Ruum emails.
    expect(within(d).queryByText(/can't send emails/)).not.toBeInTheDocument()
    expect(within(d).queryByRole('checkbox', { name: 'Send an email' })).not.toBeInTheDocument()
    await user.click(within(d).getByRole('checkbox', { name: 'Email them an invitation' }))
    expect(within(d).getByText(/Without it they cannot open it\./)).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Shared notes.md with grace@example.com. They can open it only after they join; nobody emailed them.')).toBeInTheDocument()
    expect(await within(d).findByText('grace@example.com')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.notes}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'email', email: 'grace@example.com' }, role: 'comment', notify: false })
    expect(seen.some((r) => r.path.endsWith('/invitations'))).toBe(false)
  })

  it('invites a person outside the space in plain words, and offers a link that needs no account', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    await within(d).findByText('Charles Babbage')
    // Charles has a grant, but he is not in this space: the row says he can't open it yet.
    expect(await within(d).findByText(/Can't open it yet: they haven't joined/)).toBeInTheDocument()
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'cara@example.com')
    const invite = within(form).getByRole('checkbox', { name: 'Email them an invitation' })
    expect(invite).toBeChecked()
    expect(
      within(form).getByText('cara@example.com is new here. With the invitation they make a free Ruum account, then they can open notes.md.'),
    ).toBeInTheDocument()
    // No "space" or "guest" in the words for a person outside.
    expect(within(form).queryByText(/guest/)).not.toBeInTheDocument()
    // The way without an account is offered, and leads to the link form.
    await user.click(within(form).getByRole('button', { name: 'Use a link instead' }))
    expect(within(within(d).getByRole('form', { name: 'Create a link' })).getByRole('combobox', { name: 'Access' })).toHaveFocus()
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(
      await within(d).findByText('Shared notes.md with cara@example.com. Ruum emailed them an invitation: they make a free account, then they can open it.'),
    ).toBeInTheDocument()
    const inv = seen.find((r) => r.method === 'POST' && r.path === `/v1/tenants/${ids.personal}/invitations`)
    expect(inv?.body).toEqual({
      email: 'cara@example.com',
      role: 'guest',
      message: 'I shared "notes.md" with you. Accept this invitation and make a free Ruum account to open it.',
    })
  })

  it('shares with a person of the space by user ID, so it reaches their account at once', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'Grace@Example.com')
    expect(within(form).queryByRole('checkbox', { name: /invitation/ })).not.toBeInTheDocument()
    // Grace is a Member: a share can't give her less than her role, and the form says so.
    expect(within(form).getByRole('note')).toHaveTextContent(
      'Grace Hopper is a member of Analytical Engines, so they can already manage everything here and share it. A share can\'t give them less. To let them see only some folders, make them a guest under Members.',
    )
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(
      await within(d).findByText('Shared engine-specs.txt with Grace Hopper. As member of Analytical Engines, they could already manage it.'),
    ).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.teamSpecs}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'user', id: ids.grace }, role: 'view', notify: false })
    expect(seen.some((r) => r.path.endsWith('/invitations'))).toBe(false)
    // The row names her, never "Invited, not joined yet".
    const people = within(d).getByRole('region', { name: 'People with access' })
    const row = (await within(people).findByText('Grace Hopper')).closest('li')!
    expect(row).not.toHaveTextContent('Invited, not joined yet')
    expect(row).toHaveTextContent('Member of Analytical Engines: can manage everything here, whatever this share says.')
  })

  it('shows the access that comes from the space role in a team space', async () => {
    const { d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const people = within(d).getByRole('region', { name: 'People with access' })
    const row = (await within(people).findByText(/Everyone in Analytical Engines except guests/)).closest('li')!
    expect(row).toHaveTextContent('Can manage')
    await waitFor(() => expect(row).toHaveTextContent('3 people. Owners, admins and members manage everything in Analytical Engines, and share it.'))
    expect(within(people).queryByText('Only you can see this.')).not.toBeInTheDocument()
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
    await user.selectOptions(await within(d).findByRole('combobox', { name: 'Access of Charles Babbage' }), 'view')
    await waitFor(() => expect(db.grants.find((g) => g.node_id === ids.notes)?.role).toBe('view'))
    await user.click(within(d).getByRole('button', { name: 'Remove access for Charles Babbage' }))
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
    const { d } = await openShare(`${personal}/files`, 'notes.md')
    const add = within(within(d).getByRole('form', { name: 'Add people' })).getByRole('combobox', { name: 'Access' })
    expect(within(add).getAllByRole('option').map((o) => o.textContent)).toEqual(['Can view', 'Can comment', 'Can edit'])
    const change = await within(d).findByRole('combobox', { name: 'Access of Charles Babbage' })
    expect(within(change).queryByRole('option', { name: 'Can manage' })).not.toBeInTheDocument()
  })

  it('shows a grant above the caller\'s role without controls', async () => {
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    notes.my_role = 'comment'
    const { d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Charles Babbage')).toBeInTheDocument()
    expect(within(d).queryByRole('combobox', { name: 'Access of Charles Babbage' })).not.toBeInTheDocument()
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
