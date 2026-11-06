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
  it('shares with a person by email and role', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Charles Babbage')).toBeInTheDocument()
    await user.type(within(d).getByRole('textbox', { name: 'Email' }), 'grace@example.com')
    await user.selectOptions(within(within(d).getByRole('form', { name: 'Add people' })).getByRole('combobox', { name: 'Access' }), 'comment')
    // The meaning of the role is under the menu.
    expect(within(d).getByText('Can comment: see and comment')).toBeInTheDocument()
    // Ruum sends no email for a share yet: the box is off and says why.
    expect(within(d).getByRole('checkbox', { name: 'Send an email' })).toBeDisabled()
    expect(within(d).getByRole('checkbox', { name: 'Send an email' })).not.toBeChecked()
    expect(within(d).getByText(/Ruum can't send emails for shares yet/)).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Grace Hopper')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.notes}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'email', email: 'grace@example.com' }, role: 'comment', notify: false })
  })

  it('invites a person outside the space as a guest too, so they can open the item', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    await within(d).findByText('Charles Babbage')
    // Charles has a grant, but he is not in this space: the row says he can't open it yet.
    expect(await within(d).findByText(/Not in this space: can't open it until they join as a guest/)).toBeInTheDocument()
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'cara@example.com')
    const invite = within(form).getByRole('checkbox', { name: "Also invite them to Ada Lovelace as a guest" })
    expect(invite).toBeChecked()
    expect(within(form).getByText(/cara@example\.com is not in Ada Lovelace\. They can open the item only after they join it as a guest\./)).toBeInTheDocument()
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(
      await within(d).findByText("Shared with cara@example.com. Ruum sent them an invitation to join Ada Lovelace as a guest; they can open it after they accept."),
    ).toBeInTheDocument()
    const inv = seen.find((r) => r.method === 'POST' && r.path === `/v1/tenants/${ids.personal}/invitations`)
    expect(inv?.body).toEqual({ email: 'cara@example.com', role: 'guest', message: 'I shared notes.md with you.' })
  })

  it('does not offer the guest invitation for a member of the space', async () => {
    const seen = recordRequests()
    const { user, d } = await openShare(`${team}/files`, 'engine-specs.txt')
    const form = within(d).getByRole('form', { name: 'Add people' })
    await user.type(within(form).getByRole('textbox', { name: 'Email' }), 'grace@example.com')
    expect(within(form).queryByRole('checkbox', { name: /as a guest/ })).not.toBeInTheDocument()
    await user.click(within(form).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Shared with grace@example.com.')).toBeInTheDocument()
    expect(seen.some((r) => r.path.endsWith('/invitations'))).toBe(false)
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
    await user.selectOptions(setting, 'inherit')
    await waitFor(() => expect(seen.find((r) => r.method === 'PATCH' && r.path === `/v1/nodes/${ids.documents}/sharing`)?.body).toEqual({ editors_can_share: null }))
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
