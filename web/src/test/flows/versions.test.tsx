import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp } from '../render'
import { recordRequests } from '../requests'

describe('versions panel', () => {
  async function openVersions() {
    const app = renderApp(`${personal}/files`)
    await app.user.click(await screen.findByRole('button', { name: 'Details of notes.md' }))
    await app.user.click(within(await screen.findByRole('region', { name: 'notes.md' })).getByRole('button', { name: 'Version history' }))
    return { ...app, panel: await screen.findByRole('region', { name: 'Versions of notes.md' }) }
  }

  it('lists versions with who made them, newest first', async () => {
    const { panel } = await openVersions()
    const items = await within(panel).findAllByRole('listitem')
    expect(items).toHaveLength(3)
    expect(within(items[0]).getByText('Current')).toBeInTheDocument()
    expect(within(items[0]).getByText(/Claude Desktop \(agent for Ada Lovelace\)/)).toBeInTheDocument()
    expect(within(items[2]).getByText('First draft')).toBeInTheDocument()
  })

  it('compares an old version with the current one', async () => {
    const { user, panel } = await openVersions()
    const compare = await within(panel).findAllByRole('button', { name: /^Compare version/ })
    await user.click(compare[1])
    const diff = await within(panel).findByLabelText('Changes from this version to the current one')
    expect(within(diff).getByText(/2 added, 0 removed, 2 changed/)).toBeInTheDocument()
    expect(within(diff).getByText('First idea, refined.')).toBeInTheDocument()
  })

  it('shows the words that changed inside a line, and compares two old versions', async () => {
    const seen = recordRequests()
    const { user, panel } = await openVersions()
    const compare = await within(panel).findAllByRole('button', { name: /^Compare version/ })
    await user.click(compare[1])
    let diff = await within(panel).findByLabelText('Changes from this version to the current one')
    // The detail of the changed line 3: "idea." became "idea, refined."
    const line3 = within(diff).getByText('Line 3:').closest('li')!
    expect(within(line3).getByText('idea.', { selector: 'ul del' })).toBeInTheDocument()
    expect(within(line3).getByText('idea, refined.', { selector: 'ul ins' })).toBeInTheDocument()
    expect(within(diff).getByText(/compared as text@1.0.0/)).toBeInTheDocument()

    // The first draft compared with the second version (not the current one).
    await user.selectOptions(within(panel).getByRole('combobox', { name: 'Compare with' }), '0190a000-0000-7000-8000-000000000602')
    diff = await within(panel).findByLabelText(/^Changes from this version to the version of /)
    expect(await within(diff).findByText(/1 added, 0 removed, 1 changed/)).toBeInTheDocument()
    const last = seen.filter((r) => r.path.includes('/diff?')).at(-1)!
    expect(new URL(last.path, 'http://x').searchParams.get('from')).toBe('0190a000-0000-7000-8000-000000000601')
    expect(new URL(last.path, 'http://x').searchParams.get('to')).toBe('0190a000-0000-7000-8000-000000000602')
  })

  it('compares with another diff plugin', async () => {
    const { user, panel } = await openVersions()
    const compare = await within(panel).findAllByRole('button', { name: /^Compare version/ })
    await user.click(compare[0])
    await within(panel).findByLabelText('Changes from this version to the current one')
    await user.selectOptions(await within(panel).findByRole('combobox', { name: 'Compare as' }), 'binary')
    expect(await within(panel).findByText(/compared as binary@1.0.0/)).toBeInTheDocument()
    expect(within(panel).getByText('Content:')).toBeInTheDocument()
  })

  it('restores an old version as a new current version', async () => {
    const { user, panel } = await openVersions()
    const restore = await within(panel).findAllByRole('button', { name: /^Restore version/ })
    await user.click(restore[1])
    await user.click(within(await dialog('Restore this version?')).getByRole('button', { name: 'Restore' }))
    expect(await within(panel).findByText('Version restored.')).toBeInTheDocument()
    await waitFor(() => expect(within(panel).getAllByRole('listitem')).toHaveLength(4))
    const head = db.versions.find((v) => v.id === db.nodes.find((n) => n.id === ids.notes)!.head_version)
    expect(head?.restored_from).toBe('0190a000-0000-7000-8000-000000000601')
  })

  it('labels a version', async () => {
    const { user, panel } = await openVersions()
    const label = await within(panel).findAllByRole('button', { name: /^Label version/ })
    await user.click(label[0])
    const d = await dialog('Label this version')
    await user.type(within(d).getByRole('textbox'), 'Shipped{Enter}')
    expect(await within(panel).findByText('Shipped')).toBeInTheDocument()
  })
})
