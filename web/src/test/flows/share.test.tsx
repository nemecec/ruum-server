import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp } from '../render'
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
    await user.selectOptions(within(d).getByRole('combobox', { name: 'Role' }), 'comment')
    await user.click(within(d).getByRole('button', { name: 'Share' }))
    expect(await within(d).findByText('Grace Hopper')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === `/v1/nodes/${ids.notes}/grants`)
    expect(post?.body).toEqual({ grantee: { type: 'email', email: 'grace@example.com' }, role: 'comment', notify: true })
  })

  it('changes and removes access', async () => {
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    await user.selectOptions(await within(d).findByRole('combobox', { name: 'Role of Charles Babbage' }), 'view')
    await waitFor(() => expect(db.grants.find((g) => g.node_id === ids.notes)?.role).toBe('view'))
    await user.click(within(d).getByRole('button', { name: 'Remove access of Charles Babbage' }))
    expect(await within(d).findByText('Only you.')).toBeInTheDocument()
  })

  it('creates a share link and shows its URL once', async () => {
    const { user, d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('No links.')).toBeInTheDocument()
    // The policy of the personal tenant has no edit links.
    const role = within(d).getByRole('combobox', { name: 'Link role' })
    expect(within(role).queryByRole('option', { name: 'Can edit' })).not.toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Create link' }))
    const url = await within(d).findByRole('textbox', { name: 'New link' })
    expect((url as HTMLInputElement).value).toMatch(/\/s\/[0-9a-f]{32}$/)
    expect(await within(d).findByText(/Can view link/)).toBeInTheDocument()
  })

  it('revokes a share link after a confirmation', async () => {
    const { user } = await openShare(`${personal}/files/${ids.documents}`, 'report.txt')
    await user.click(await screen.findByRole('button', { name: 'Revoke can view link' }))
    const confirm = await dialog('Revoke the link?')
    await user.click(within(confirm).getByRole('button', { name: 'Revoke link' }))
    const d = await dialog('Share report.txt')
    expect(await within(d).findByText('No links.')).toBeInTheDocument()
  })

  it('does not list revoked links', async () => {
    db.shareLinks[0].revoked_at = '2026-11-02T00:00:00Z'
    const { d } = await openShare(`${personal}/files/${ids.documents}`, 'report.txt')
    expect(await within(d).findByText('No links.')).toBeInTheDocument()
    expect(within(d).queryByRole('button', { name: 'Revoke can view link' })).not.toBeInTheDocument()
  })

  it('offers only roles up to the caller\'s own role', async () => {
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    notes.my_role = 'edit'
    const { d } = await openShare(`${personal}/files`, 'notes.md')
    const add = within(d).getByRole('combobox', { name: 'Role' })
    expect(within(add).getAllByRole('option').map((o) => o.textContent)).toEqual(['Can view', 'Can comment', 'Can edit'])
    const change = await within(d).findByRole('combobox', { name: 'Role of Charles Babbage' })
    expect(within(change).queryByRole('option', { name: 'Can manage' })).not.toBeInTheDocument()
  })

  it('shows a grant above the caller\'s role without controls', async () => {
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    notes.my_role = 'comment'
    const { d } = await openShare(`${personal}/files`, 'notes.md')
    expect(await within(d).findByText('Charles Babbage')).toBeInTheDocument()
    expect(within(d).queryByRole('combobox', { name: 'Role of Charles Babbage' })).not.toBeInTheDocument()
    const link = within(d).getByRole('combobox', { name: 'Link role' })
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
    expect(within(d).getByText('(from a parent folder)')).toBeInTheDocument()
    expect(within(d).queryByRole('combobox', { name: 'Role of grace@example.com' })).not.toBeInTheDocument()
  })
})
