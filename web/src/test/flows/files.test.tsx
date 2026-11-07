import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { browser } from '../../app/browser'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp, choose } from '../render'
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
    expect(rows.map((r) => (within(r).getByRole('rowheader').querySelector('a, button')?.textContent))).toEqual(['Documents', 'Photos', 'notes.md'])

    await user.click(screen.getByRole('link', { name: 'Documents' }))
    expect(await screen.findByRole('heading', { name: 'Documents', level: 1 })).toBeInTheDocument()
    const crumbs = screen.getByRole('navigation', { name: 'Breadcrumbs' })
    // The top of the tree is called "Files", never the space name (ia.md section 1).
    expect(within(crumbs).getByRole('link', { name: 'Files' })).toBeInTheDocument()
    expect(within(crumbs).getByText('Documents')).toHaveAttribute('aria-current', 'page')
    // A trashed node is not listed.
    expect(screen.queryByText('old-draft.txt')).not.toBeInTheDocument()
  })

  it('sorts by name, size and change, folders first, with aria-sort', async () => {
    const { user } = renderApp(`${personal}/files/${ids.documents}`)
    const table = await screen.findByRole('table', { name: 'Contents of this folder' })
    const names = () => within(table).getAllByRole('row').slice(1).map((r) => (within(r).getByRole('rowheader').querySelector('a, button')?.textContent))
    expect(names()).toEqual(['plan.txt', 'report.txt'])
    await user.click(within(table).getByRole('button', { name: 'Name' }))
    expect(names()).toEqual(['report.txt', 'plan.txt'])
    expect(within(table).getByRole('columnheader', { name: /Name/ })).toHaveAttribute('aria-sort', 'descending')
    await user.click(within(table).getByRole('button', { name: 'Size' }))
    expect(within(table).getByRole('columnheader', { name: /Size/ })).toHaveAttribute('aria-sort', 'descending')
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
    // The field has the focus and starts empty, with an example as the placeholder.
    expect(input).toHaveFocus()
    expect(input).toHaveValue('')
    await user.keyboard('Invoices{Enter}')
    expect(await screen.findByRole('link', { name: 'Invoices' })).toBeInTheDocument()
    expect(await screen.findByText('Folder Invoices created.')).toBeInTheDocument()
  })

  it('shows a name conflict in the dialog', async () => {
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('button', { name: 'New folder' }))
    const d = await dialog('New folder')
    const input = within(d).getByRole('textbox', { name: 'Folder name' })
    await user.type(input, 'Photos{Enter}')
    // Copy for people (copy.md section 4), not the API's message.
    expect(await within(d).findByRole('alert')).toHaveTextContent("There's already an item called Photos here. Pick another name.")
    // The code and the API's message stay in the "Details" disclosure.
    expect(within(within(d).getByRole('alert')).getByText('Details')).toBeInTheDocument()
  })

  it('uploads a file in chunks: chunking parameters, createUpload, the missing chunks, complete', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    const file = new File(['hello ruum'], 'hello.txt', { type: 'text/plain' })
    await user.upload(screen.getByLabelText('Upload files'), file)
    expect(await screen.findByRole('button', { name: 'hello.txt' })).toBeInTheDocument()
    expect(await screen.findByText('Uploaded hello.txt.')).toBeInTheDocument()
    expect(within(screen.getByRole('region', { name: 'Uploads' })).getByText('Done')).toBeInTheDocument()

    const calls = seen.filter((r) => r.path.startsWith('/v1/uploads')).map((r) => `${r.method} ${r.path.replace(/[0-9a-f]{64}/g, ':hash').replace(/[0-9a-f-]{36}/g, ':id')}`)
    expect(calls).toEqual([
      'GET /v1/uploads/params?size=10&content_type=text%2Fplain',
      'POST /v1/uploads',
      'PUT /v1/uploads/:id/chunks/:hash',
      'POST /v1/uploads/:id/complete',
    ])
    const create = seen.find((r) => r.method === 'POST' && r.path === '/v1/uploads')!
    expect(create.body).toMatchObject({
      target: 'new_file',
      parent_id: ids.personalRoot,
      name: 'hello.txt',
      // The list on the screen can be a moment old: a taken name comes back as name_taken.
      on_conflict: 'fail',
      size: 10,
      chunking: 'fastcdc-2020-16k-32k-64k',
      content_type: 'text/plain',
      // BLAKE3 of "hello ruum"
      content_hash: expect.stringMatching(/^[0-9a-f]{64}$/),
    })
    const put = seen.find((r) => r.method === 'PUT')!
    expect(put.body).toBe('hello ruum')
    expect(put.headers.get('Content-Type')).toBe('application/octet-stream')
    expect(put.headers.get('Ruum-Tenant')).toBe(ids.personal)
    expect(put.headers.get('Idempotency-Key')).toBeTruthy()
    // No upload of whole files through the API: no createNode and no putContent.
    expect(seen.some((r) => r.path.endsWith('/content') || (r.method === 'POST' && r.path === '/v1/nodes'))).toBe(false)
  })

  it('sends no chunk that the tenant already stores (dedup)', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    await user.upload(screen.getByLabelText('Upload files'), new File(['same bytes'], 'a.txt', { type: 'text/plain' }))
    expect(await screen.findByRole('button', { name: 'a.txt' })).toBeInTheDocument()
    await user.upload(screen.getByLabelText('Upload files'), new File(['same bytes'], 'b.txt', { type: 'text/plain' }))
    expect(await screen.findByRole('button', { name: 'b.txt' })).toBeInTheDocument()
    expect(seen.filter((r) => r.method === 'PUT' && r.path.includes('/chunks/'))).toHaveLength(1)
    expect(seen.filter((r) => r.path.endsWith('/complete'))).toHaveLength(2)
  })

  it('uploads a new version of a file, based on the current head', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    const head = db.nodes.find((n) => n.id === ids.notes)!.head_version
    await user.upload(within(details).getByLabelText('New version of notes.md'), new File(['# Notes v2'], 'notes.md', { type: 'text/markdown' }))
    expect(await screen.findByText('Uploaded notes.md.')).toBeInTheDocument()
    const create = seen.find((r) => r.method === 'POST' && r.path === '/v1/uploads')!
    expect(create.body).toMatchObject({ target: 'new_version', node_id: ids.notes, base_version: head })
    const notes = db.nodes.find((n) => n.id === ids.notes)!
    expect(notes.head_version).not.toBe(head)
    expect(db.contents.get(notes.head_version!)).toBe('# Notes v2')
  })

  it('shows a failed upload in the list', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    // Someone else writes a new version first.
    db.nodes.find((n) => n.id === ids.notes)!.head_version = db.versions[0].id
    await user.upload(within(details).getByLabelText('New version of notes.md'), new File(['late'], 'notes.md'))
    const uploads = screen.getByRole('region', { name: 'Uploads' })
    expect(await within(uploads).findByRole('alert')).toHaveTextContent("Didn't upload: Someone else changed notes.md a moment ago.")
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

  it('previews through an inline content domain URL in a new tab', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/files`)
    const tab = { closed: false, opener: {}, location: { href: 'about:blank' }, close: vi.fn() }
    const openBlank = vi.spyOn(browser, 'openBlank').mockReturnValue(tab as unknown as Window)
    const details = await openDetails(user, 'notes.md')
    await user.click(within(details).getByRole('button', { name: 'Preview' }))
    await waitFor(() => expect(tab.location.href).toMatch(/^data:/))
    expect(openBlank).toHaveBeenCalledTimes(1)
    expect(tab.opener).toBeNull()
    const call = seen.find((r) => r.path.includes('/content-url'))!
    expect(new URL(call.path, 'http://x').searchParams.get('disposition')).toBe('inline')
  })

  it('renames a file', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    await choose(user, 'More actions for notes.md', 'Rename', details)
    const d = await dialog('Rename notes.md')
    const input = within(d).getByRole<HTMLInputElement>('textbox', { name: 'Name' })
    // The name without its extension is selected when the dialog opens: typing keeps ".md".
    expect(input).toHaveFocus()
    expect([input.selectionStart, input.selectionEnd]).toEqual([0, 'notes'.length])
    await user.keyboard('ideas')
    expect(input).toHaveValue('ideas.md')
    await user.click(within(d).getByRole('button', { name: 'Rename' }))
    expect(await screen.findByRole('button', { name: 'ideas.md' })).toBeInTheDocument()
  })

  it('moves a file into another folder', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    await choose(user, /^More actions for/, 'Move', details)
    const d = await dialog('Move notes.md')
    // The current folder is not a target.
    expect(await within(d).findByText("It's already here.")).toBeInTheDocument()
    await user.click(await within(d).findByRole('button', { name: 'Photos' }))
    await user.click(await within(d).findByRole('button', { name: 'Move here' }))
    await waitFor(() => expect(screen.queryByRole('button', { name: 'notes.md' })).not.toBeInTheDocument())
    expect(db.nodes.find((n) => n.id === ids.notes)?.parent_id).toBe(ids.photos)
    // The toast names the folder and can undo the move.
    expect(await screen.findByText('Moved notes.md to Photos.')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Undo' }))
    expect(await screen.findByText('Moved notes.md back.')).toBeInTheDocument()
    expect(db.nodes.find((n) => n.id === ids.notes)?.parent_id).toBe(ids.personalRoot)
  })

  it('moves a file to the trash and restores it from there', async () => {
    const { user } = renderApp(`${personal}/files`)
    const details = await openDetails(user, 'notes.md')
    // The trash can be undone, so there is no confirmation; the toast offers Undo.
    await choose(user, 'More actions for notes.md', 'Move to trash', details)
    await waitFor(() => expect(screen.queryByRole('button', { name: 'notes.md' })).not.toBeInTheDocument())
    expect(await screen.findByText('Moved notes.md to trash.')).toBeInTheDocument()

    await user.click(screen.getByRole('link', { name: 'Trash' }))
    await user.click(await screen.findByRole('button', { name: 'Restore notes.md' }))
    expect(await screen.findByText('Restored notes.md to Files.')).toBeInTheDocument()
    expect(db.nodes.find((n) => n.id === ids.notes)?.trashed_at).toBeNull()
  })

  it('deletes from the trash for good after a confirmation', async () => {
    const { user } = renderApp(`${personal}/trash`)
    await choose(user, 'More actions for old-draft.txt', 'Delete for good')
    const d = await dialog('Delete old-draft.txt for good?')
    await user.click(within(d).getByRole('button', { name: 'Delete for good' }))
    expect(await screen.findByText('The trash is empty')).toBeInTheDocument()
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
    const row = (await screen.findByRole('rowheader', { name: /^Difference engine\.txt/ })).closest('tr')!
    expect(within(row).getByText('Can comment')).toBeInTheDocument()
    expect(within(row).getByText('Charles Babbage')).toBeInTheDocument()
  })

  it('opens and downloads shared items in their own tenant, not the tenant of the page', async () => {
    const seen = recordRequests()
    const { user, router, download } = renderApp(`${personal}/shared`)
    // An item of another tenant (Ada is a guest there) is listed on the page of her personal tenant.
    const row = (await screen.findByRole('rowheader', { name: /^Engine drawings/ })).closest('tr')!
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
    expect(await screen.findByText('You see only what people in Babbage & Co shared with you.')).toBeInTheDocument()
    // Only the items of this tenant.
    expect(screen.queryByText('Difference engine.txt')).not.toBeInTheDocument()
    await user.click(await screen.findByRole('link', { name: 'Engine drawings' }))
    expect(await screen.findByRole('button', { name: 'gears.txt' })).toBeInTheDocument()
  })

  it('moves a file of a guest: the picker goes up to the shared folders, not the hidden root', async () => {
    const { user } = renderApp(`/t/${ids.guest}/files/${ids.guestDrawings}`)
    const details = await openDetails(user, 'gears.txt')
    await choose(user, /^More actions for/, 'Move', details)
    const d = await dialog('Move gears.txt')
    expect(await within(d).findByRole('button', { name: 'Sketches' })).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Folders shared with you' }))
    expect(await within(d).findByText('Folders shared with you')).toBeInTheDocument()
    expect(within(d).queryByRole('button', { name: 'Move here' })).not.toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Engine drawings' }))
    await user.click(await within(d).findByRole('button', { name: 'Sketches' }))
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
