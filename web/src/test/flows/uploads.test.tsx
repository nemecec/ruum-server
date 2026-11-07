import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp, choose } from '../render'
import { recordRequests } from '../requests'

describe('uploads, conflicts and the file list', () => {
  it('asks once when a name is already there, and uploads a new version by default', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.upload(screen.getByLabelText('Upload files'), new File(['# Notes v3'], 'notes.md', { type: 'text/markdown' }))
    const d = await dialog('notes.md is already here')
    expect(within(d).getByRole('radio', { name: 'Save as a new version' })).toBeChecked()
    expect(within(d).getByText('The file keeps its name. The earlier version stays in Versions.')).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Continue' }))
    expect(await screen.findByText(/^Uploaded notes\.md\./)).toBeInTheDocument()
    const create = seen.find((r) => r.method === 'POST' && r.path === '/v1/uploads')!
    expect(create.body).toMatchObject({ target: 'new_version', node_id: ids.notes })
    // No second file.
    expect(db.nodes.filter((n) => n.parent_id === ids.personalRoot && n.name.startsWith('notes'))).toHaveLength(1)
  })

  it('saves a new version that never joins the person\'s own version of the last minutes (C-8)', async () => {
    // The head is Ada's own unnamed save of a minute ago: the server would join an upload into it
    // by default. The web UI sends `coalesce: false`, so it needs no name and no warning.
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    const head = db.versions.find((v) => v.id === notes.head_version)!
    head.actor = { kind: 'user', id: ids.ada, display_name: 'Ada Lovelace' }
    head.on_behalf_of = null
    notes.updated_by = { actor: head.actor, on_behalf_of: null }
    notes.updated_at = new Date(Date.now() - 60_000).toISOString()
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.upload(screen.getByLabelText('Upload files'), new File(['# v4'], 'notes.md', { type: 'text/markdown' }))
    const d = await dialog('notes.md is already here')
    expect(within(d).getByRole('radio', { name: 'Save as a new version' })).toBeChecked()
    expect(within(d).getByText('The file keeps its name. The earlier version stays in Versions.')).toBeInTheDocument()
    expect(within(d).queryByRole('textbox')).not.toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Continue' }))
    expect(await screen.findByText(/^Uploaded notes\.md\.$/)).toBeInTheDocument()
    expect(seen.find((r) => r.method === 'POST' && r.path === '/v1/uploads')?.body).toMatchObject({
      target: 'new_version',
      node_id: ids.notes,
      coalesce: false,
    })
    // No version is renamed to keep it: the earlier version stays as it is.
    expect(seen.some((r) => r.method === 'PUT' && r.path.endsWith('/label'))).toBe(false)
    expect(db.versions.find((v) => v.id === head.id)?.label ?? null).toBeNull()
  })

  it('asks when the server answers that a name is taken that the list on the screen did not show yet', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    // The name is taken after the list was read: by the upload just before, or another tab.
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    const late = { ...notes, id: crypto.randomUUID(), name: 'late.txt' }
    db.nodes.push(late)
    await user.upload(screen.getByLabelText('Upload files'), new File(['mine'], 'Late.txt', { type: 'text/plain' }))
    const d = await dialog('Late.txt is already here')
    expect(seen.find((r) => r.method === 'POST' && r.path === '/v1/uploads')?.body).toMatchObject({ target: 'new_file', on_conflict: 'fail' })
    // The file left the upload list while the question is open; it is not a failure.
    expect(screen.queryByText(/failed/i)).not.toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Continue' }))
    expect(await screen.findByText(/^Uploaded late\.txt\./)).toBeInTheDocument()
    const creates = seen.filter((r) => r.method === 'POST' && r.path === '/v1/uploads')
    expect(creates.at(-1)?.body).toMatchObject({ target: 'new_version', node_id: late.id })
    // Nothing was renamed or replaced without the answer.
    expect(db.nodes.filter((n) => n.parent_id === ids.personalRoot && /^late/i.test(n.name))).toHaveLength(1)
  })

  it('keeps both on "Keep both" also when the name was taken after the list was read', async () => {
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    db.nodes.push({ ...notes, id: crypto.randomUUID(), name: 'late.txt' })
    await user.upload(screen.getByLabelText('Upload files'), new File(['mine'], 'late.txt', { type: 'text/plain' }))
    const d = await dialog('late.txt is already here')
    await user.click(within(d).getByRole('radio', { name: 'Keep both, as two separate files' }))
    await user.click(within(d).getByRole('button', { name: 'Continue' }))
    const panel = await screen.findByRole('region', { name: 'Uploads' })
    expect(await within(panel).findByText('Done, saved as late (2).txt')).toBeInTheDocument()
  })

  it('keeps both, and shows the name the file was saved under', async () => {
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.upload(screen.getByLabelText('Upload files'), new File(['other'], 'notes.md', { type: 'text/markdown' }))
    const d = await dialog('notes.md is already here')
    await user.click(within(d).getByRole('radio', { name: 'Keep both, as two separate files' }))
    await user.click(within(d).getByRole('button', { name: 'Continue' }))
    const panel = await screen.findByRole('region', { name: 'Uploads' })
    expect(await within(panel).findByText('Done, saved as notes (2).md')).toBeInTheDocument()
    expect(within(panel).getByText('notes (2).md')).toBeInTheDocument()
  })

  it('skips the files whose names are taken', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.upload(screen.getByLabelText('Upload files'), [
      new File(['a'], 'notes.md', { type: 'text/markdown' }),
      new File(['b'], 'fresh.txt', { type: 'text/plain' }),
    ])
    const d = await dialog('notes.md is already here')
    await user.click(within(d).getByRole('radio', { name: 'Skip' }))
    await user.click(within(d).getByRole('button', { name: 'Continue' }))
    expect(await screen.findByRole('button', { name: 'fresh.txt' })).toBeInTheDocument()
    expect(seen.filter((r) => r.method === 'POST' && r.path === '/v1/uploads')).toHaveLength(1)
  })

  it('keeps the upload panel when the person opens another page', async () => {
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.upload(screen.getByLabelText('Upload files'), new File(['hello'], 'travel.txt', { type: 'text/plain' }))
    await user.click(screen.getByRole('link', { name: 'Agents' }))
    expect(await screen.findByRole('heading', { name: 'Agents', level: 1 })).toBeInTheDocument()
    const panel = screen.getByRole('region', { name: 'Uploads' })
    expect(within(panel).getByText('travel.txt')).toBeInTheDocument()
    expect(await within(panel).findByText('1 upload done')).toBeInTheDocument()
  })

  it('"Upload new version" always makes a new version, without asking (C-8)', async () => {
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    const head = db.versions.find((v) => v.id === notes.head_version)!
    head.actor = { kind: 'user', id: ids.ada, display_name: 'Ada Lovelace' }
    head.on_behalf_of = null
    notes.updated_by = { actor: head.actor, on_behalf_of: null }
    notes.updated_at = new Date(Date.now() - 60_000).toISOString()
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('button', { name: 'Details of notes.md' }))
    const details = await screen.findByRole('region', { name: 'notes.md' })
    await user.upload(within(details).getByLabelText('New version of notes.md'), new File(['# v4'], 'notes.md'))
    expect(await screen.findByText(/^Uploaded notes\.md\.$/)).toBeInTheDocument()
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(seen.find((r) => r.method === 'POST' && r.path === '/v1/uploads')?.body).toMatchObject({
      target: 'new_version',
      node_id: ids.notes,
      base_version: head.id,
      coalesce: false,
    })
    expect(seen.some((r) => r.method === 'PUT' && r.path.endsWith('/label'))).toBe(false)
    expect(db.nodes.find((n) => n.id === ids.notes)?.head_version).not.toBe(head.id)
  })

  it('selects several items and moves them to the trash with Undo', async () => {
    const { user } = renderApp(`${personal}/files/${ids.documents}`)
    await user.click(await screen.findByRole('checkbox', { name: 'Select plan.txt' }))
    await user.click(screen.getByRole('checkbox', { name: 'Select report.txt' }))
    const bar = screen.getByRole('region', { name: 'Selection' })
    expect(within(bar).getByText('2 items selected')).toBeInTheDocument()
    await user.click(within(bar).getByRole('button', { name: 'Move to trash' }))
    expect(await screen.findByText('Moved 2 items to trash.')).toBeInTheDocument()
    await waitFor(() => expect(screen.queryByRole('button', { name: 'plan.txt' })).not.toBeInTheDocument())
    await user.click(screen.getByRole('button', { name: 'Undo' }))
    expect(await screen.findByText('Restored 2 items.')).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'plan.txt' })).toBeInTheDocument()
  })

  it('moves several items at once', async () => {
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('checkbox', { name: 'Select notes.md' }))
    await user.click(screen.getByRole('checkbox', { name: 'Select Photos' }))
    await user.click(within(screen.getByRole('region', { name: 'Selection' })).getByRole('button', { name: 'Move' }))
    const d = await dialog('Move 2 items')
    // A folder can't go inside itself.
    expect(await within(d).findByRole('button', { name: 'Photos' })).toBeDisabled()
    await user.click(within(d).getByRole('button', { name: 'Documents' }))
    await user.click(await within(d).findByRole('button', { name: 'Move here' }))
    expect(await screen.findByText('Moved 2 items to Documents.')).toBeInTheDocument()
    expect(db.nodes.find((n) => n.id === ids.notes)?.parent_id).toBe(ids.documents)
    expect(db.nodes.find((n) => n.id === ids.photos)?.parent_id).toBe(ids.documents)
  })

  it('makes a folder inside the move picker', async () => {
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('button', { name: 'Details of notes.md' }))
    await choose(user, 'More actions for notes.md', 'Move', await screen.findByRole('region', { name: 'notes.md' }))
    const d = await dialog('Move notes.md')
    await user.click(await within(d).findByRole('button', { name: 'New folder' }))
    const nf = await dialog('New folder')
    await user.keyboard('Archive{Enter}')
    await waitFor(() => expect(nf).not.toBeInTheDocument())
    expect(await within(d).findByText('Archive', { selector: '[aria-current]' })).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Move here' }))
    expect(await screen.findByText('Moved notes.md to Archive.')).toBeInTheDocument()
  })
})

describe('first visit', () => {
  it('shows the welcome card on an empty personal space, until Close', async () => {
    db.nodes = db.nodes.filter((n) => n.tenant_id !== ids.personal || !n.parent_id)
    const { user, unmount } = renderApp(`${personal}/files`)
    const card = await screen.findByRole('region', { name: 'Welcome to Ruum, Ada' })
    expect(within(card).getByText('Every change is kept as a version, so you can go back.')).toBeInTheDocument()
    expect(screen.getByText('Your files go here')).toBeInTheDocument()
    await user.click(within(card).getByRole('button', { name: 'Close' }))
    expect(screen.queryByRole('region', { name: 'Welcome to Ruum, Ada' })).not.toBeInTheDocument()
    unmount()
    renderApp(`${personal}/files`)
    await screen.findByText('Your files go here')
    expect(screen.queryByRole('region', { name: 'Welcome to Ruum, Ada' })).not.toBeInTheDocument()
  })

  it('shows it again with ?welcome=1, and its button opens Connect an agent', async () => {
    const { user } = renderApp(`${personal}/files?welcome=1`)
    const card = await screen.findByRole('region', { name: 'Welcome to Ruum, Ada' })
    await user.click(within(card).getByRole('button', { name: 'Connect an agent' }))
    expect(await screen.findByRole('heading', { name: 'Connect an agent', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Server address' })).toHaveValue(`${window.location.origin}/mcp`)
  })
})
