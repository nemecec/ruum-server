import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp } from '../render'
import { recordRequests } from '../requests'

async function openDetails(user: ReturnType<typeof renderApp>['user'], name: string) {
  await user.click(await screen.findByRole('button', { name: `Details of ${name}` }))
  return screen.findByRole('region', { name })
}

describe('file browser', () => {
  it('lists a folder, opens a subfolder and shows breadcrumbs', async () => {
    const { user } = renderApp(`${personal}/files`)
    const table = await screen.findByRole('table', { name: 'Contents of this folder' })
    const rows = within(table).getAllByRole('row').slice(1)
    expect(rows.map((r) => within(r).getByRole('rowheader').textContent)).toEqual(['Documents', 'Photos', 'notes.md'])

    await user.click(screen.getByRole('link', { name: 'Documents' }))
    expect(await screen.findByRole('heading', { name: 'Documents', level: 1 })).toBeInTheDocument()
    const crumbs = screen.getByRole('navigation', { name: 'Breadcrumbs' })
    expect(within(crumbs).getByRole('link', { name: 'Ada Lovelace' })).toBeInTheDocument()
    expect(within(crumbs).getByText('Documents')).toHaveAttribute('aria-current', 'page')
    // A trashed node is not listed.
    expect(screen.queryByText('old-draft.txt')).not.toBeInTheDocument()
  })

  it('works with the keyboard: the details panel takes the focus', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await screen.findByRole('button', { name: 'Details of notes.md' })
    details.focus()
    await user.keyboard('{Enter}')
    const heading = await screen.findByRole('heading', { name: 'notes.md', level: 2 })
    await waitFor(() => expect(heading).toHaveFocus())
    await user.tab()
    expect(screen.getByRole('button', { name: 'Close details' })).toHaveFocus()
  })

  it('creates a folder', async () => {
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('button', { name: 'New folder' }))
    const d = await dialog('New folder')
    const input = within(d).getByRole('textbox', { name: 'Folder name' })
    await user.clear(input)
    await user.type(input, 'Invoices{Enter}')
    expect(await screen.findByRole('link', { name: 'Invoices' })).toBeInTheDocument()
    expect(screen.getByRole('status', { name: '' })).toHaveTextContent('Folder Invoices created.')
  })

  it('shows a name conflict in the dialog', async () => {
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('button', { name: 'New folder' }))
    const d = await dialog('New folder')
    const input = within(d).getByRole('textbox', { name: 'Folder name' })
    await user.clear(input)
    await user.type(input, 'Photos{Enter}')
    expect(await within(d).findByRole('alert')).toHaveTextContent('A node named "Photos" already exists here.')
  })

  it('uploads a small file with createNode and putContent', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    const file = new File(['hello ruum'], 'hello.txt', { type: 'text/plain' })
    await user.upload(screen.getByLabelText('Upload files'), file)
    expect(await screen.findByRole('button', { name: 'hello.txt' })).toBeInTheDocument()
    const put = seen.find((r) => r.method === 'PUT' && r.path.endsWith('/content'))
    expect(put?.body).toBe('hello ruum')
    expect(put?.headers.get('Content-Type')).toBe('text/plain')
    expect(put?.headers.get('Idempotency-Key')).toBeTruthy()
  })

  it('downloads through the content domain URL, not the API', async () => {
    const seen = recordRequests()
    const { user, download } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    await user.click(within(details).getByRole('button', { name: 'Download' }))
    await waitFor(() => expect(download).toHaveBeenCalledTimes(1))
    expect(download.mock.calls[0][1]).toBe('notes.md')
    expect(seen.some((r) => r.path.includes('/content-url'))).toBe(true)
    expect(seen.some((r) => r.method === 'GET' && r.path.endsWith('/content'))).toBe(false)
  })

  it('renames a file', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    await user.click(within(details).getByRole('button', { name: 'Rename' }))
    const d = await dialog('Rename notes.md')
    const input = within(d).getByRole('textbox', { name: 'New name' })
    await user.clear(input)
    await user.type(input, 'ideas.md')
    await user.click(within(d).getByRole('button', { name: 'Rename' }))
    expect(await screen.findByRole('button', { name: 'ideas.md' })).toBeInTheDocument()
  })

  it('moves a file into another folder', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    await user.click(within(details).getByRole('button', { name: 'Move' }))
    const d = await dialog('Move notes.md')
    await user.click(await within(d).findByRole('button', { name: 'Photos/' }))
    await user.click(await within(d).findByRole('button', { name: 'Move here' }))
    await waitFor(() => expect(screen.queryByRole('button', { name: 'notes.md' })).not.toBeInTheDocument())
    expect(db.nodes.find((n) => n.id === ids.notes)?.parent_id).toBe(ids.photos)
  })

  it('moves a file to the trash and restores it from there', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    await user.click(within(details).getByRole('button', { name: 'Move to trash' }))
    const d = await dialog('Move notes.md to the trash?')
    await user.click(within(d).getByRole('button', { name: 'Move to trash' }))
    await waitFor(() => expect(screen.queryByRole('button', { name: 'notes.md' })).not.toBeInTheDocument())

    await user.click(screen.getByRole('link', { name: 'Trash' }))
    await user.click(await screen.findByRole('button', { name: 'Restore notes.md' }))
    expect(await screen.findByText('notes.md restored.')).toBeInTheDocument()
    expect(db.nodes.find((n) => n.id === ids.notes)?.trashed_at).toBeNull()
  })

  it('deletes from the trash for good after a confirmation', async () => {
    const { user } = renderApp(`${personal}/trash`)
    await user.click(await screen.findByRole('button', { name: 'Delete old-draft.txt for good' }))
    const d = await dialog('Delete old-draft.txt for good?')
    await user.click(within(d).getByRole('button', { name: 'Delete for good' }))
    expect(await screen.findByText('The trash is empty.')).toBeInTheDocument()
  })

  it('adds and removes tags', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    const tags = within(details).getByRole('region', { name: 'Tags' })
    expect(await within(tags).findByText('important')).toBeInTheDocument()

    await user.selectOptions(await within(tags).findByRole('combobox', { name: 'Add a tag' }), 'draft')
    expect(await within(tags).findByText('draft')).toBeInTheDocument()

    await user.click(within(tags).getByRole('button', { name: 'Remove tag important' }))
    await waitFor(() => expect(within(within(tags).getByRole('list')).queryByText('important')).not.toBeInTheDocument())
  })

  it('opens the details of a search hit in its folder', async () => {
    const { user } = renderApp(`${personal}/search?q=plan`)
    await user.click(await screen.findByRole('link', { name: 'plan.txt' }))
    expect(await screen.findByRole('region', { name: 'plan.txt' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Documents', level: 1 })).toBeInTheDocument()
  })

  it('lists what others shared with me', async () => {
    renderApp(`${personal}/shared`)
    const row = (await screen.findByRole('rowheader', { name: 'Difference engine.txt' })).closest('tr')!
    expect(within(row).getByText('Can comment')).toBeInTheDocument()
    expect(within(row).getByText('Charles Babbage')).toBeInTheDocument()
  })

  it('opens and downloads shared items in their own tenant, not the tenant of the page', async () => {
    const seen = recordRequests()
    const { user, router, download } = renderApp(`${personal}/shared`)
    // An item of another tenant (Ada is a guest there) is listed on the page of her personal tenant.
    const row = (await screen.findByRole('rowheader', { name: 'Engine drawings' })).closest('tr')!
    expect(within(row).getByText('Babbage & Co')).toBeInTheDocument()
    expect(seen.find((r) => r.path === '/v1/shared-with-me')?.headers.has('Ruum-Tenant')).toBe(false)

    await user.click(screen.getByRole('button', { name: 'Download Difference engine.txt' }))
    await waitFor(() => expect(download).toHaveBeenCalledTimes(1))
    expect(seen.find((r) => r.path.startsWith(`/v1/nodes/${ids.sharedDoc}/content-url`))?.headers.get('Ruum-Tenant')).toBe(ids.personal)

    await user.click(within(row).getByRole('link', { name: 'Engine drawings' }))
    expect(router.state.location.pathname).toBe(`/t/${ids.guest}/files/${ids.guestDrawings}`)
    expect(await screen.findByRole('heading', { name: 'Engine drawings', level: 1 })).toBeInTheDocument()
    expect(seen.find((r) => r.path === `/v1/nodes/${ids.guestDrawings}`)?.headers.get('Ruum-Tenant')).toBe(ids.guest)
  })

  it('opens a folder shared inside a tree whose parents the guest cannot see', async () => {
    const { user } = renderApp(`/t/${ids.guest}/files/${ids.guestDrawings}`)
    expect(await screen.findByRole('heading', { name: 'Engine drawings', level: 1 })).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'gears.txt' })).toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()

    // The breadcrumbs start at the highest folder the guest can see.
    await user.click(screen.getByRole('link', { name: 'Sketches' }))
    expect(await screen.findByRole('heading', { name: 'Sketches', level: 1 })).toBeInTheDocument()
    const crumbs = screen.getByRole('navigation', { name: 'Breadcrumbs' })
    expect(within(crumbs).getAllByRole('listitem').map((li) => li.textContent)).toEqual(['Engine drawings', 'Sketches'])
  })

  it('shows a guest the shared items instead of the root it cannot see', async () => {
    const { user } = renderApp(`/t/${ids.guest}/files`)
    expect(await screen.findByText('You are a guest in this tenant. You see only what other people shared with you.')).toBeInTheDocument()
    // Only the items of this tenant.
    expect(screen.queryByText('Difference engine.txt')).not.toBeInTheDocument()
    await user.click(await screen.findByRole('link', { name: 'Engine drawings' }))
    expect(await screen.findByRole('button', { name: 'gears.txt' })).toBeInTheDocument()
  })

  it('moves a file of a guest: the picker goes up to the shared folders, not the hidden root', async () => {
    const { user } = renderApp(`/t/${ids.guest}/files/${ids.guestDrawings}`)
    const details = await openDetails(user, 'gears.txt')
    await user.click(within(details).getByRole('button', { name: 'Move' }))
    const d = await dialog('Move gears.txt')
    expect(await within(d).findByRole('button', { name: 'Sketches/' })).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: '.. (up one level)' }))
    expect(await within(d).findByText('Folders shared with you')).toBeInTheDocument()
    expect(within(d).queryByRole('button', { name: 'Move here' })).not.toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Engine drawings/' }))
    await user.click(await within(d).findByRole('button', { name: 'Sketches/' }))
    await user.click(await within(d).findByRole('button', { name: 'Move here' }))
    await waitFor(() => expect(db.nodes.find((n) => n.id === ids.guestGears)?.parent_id).toBe(ids.guestSketches))
  })

  it('pages a large folder with "Load more"', async () => {
    for (let i = 0; i < 105; i++) {
      db.nodes.push({ ...db.nodes.find((n) => n.id === ids.plan)!, id: crypto.randomUUID(), parent_id: ids.photos, name: `img-${String(i).padStart(3, '0')}.jpg` })
    }
    const { user } = renderApp(`${personal}/files/${ids.photos}`)
    const table = await screen.findByRole('table', { name: 'Contents of this folder' })
    expect(within(table).getAllByRole('row')).toHaveLength(101)
    expect(screen.getByText('There are more items.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Load more' }))
    await waitFor(() => expect(within(table).getAllByRole('row')).toHaveLength(106))
    expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument()
  })
})
