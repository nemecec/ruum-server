import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp } from '../render'

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
