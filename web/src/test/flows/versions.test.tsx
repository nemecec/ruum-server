import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp, choose } from '../render'
import { recordRequests } from '../requests'

describe('versions panel', () => {
  async function openVersions() {
    const app = renderApp(`${personal}/files`)
    await app.user.click(await screen.findByRole('button', { name: 'Details of notes.md' }))
    await choose(app.user, 'More actions for notes.md', 'Versions', await screen.findByRole('region', { name: 'notes.md' }))
    return { ...app, panel: await screen.findByRole('region', { name: 'Versions of notes.md' }) }
  }

  it('lists versions with who made them, newest first', async () => {
    const { panel } = await openVersions()
    const items = await within(panel).findAllByRole('listitem')
    expect(items).toHaveLength(3)
    expect(within(items[0]).getByText('Current')).toBeInTheDocument()
    // The agent shows with its name, the person it works for, and the agent mark.
    expect(within(items[0]).getByText(/Claude Desktop \(agent of Ada Lovelace\)/)).toBeInTheDocument()
    expect(within(items[0]).getByText('(an agent)')).toBeInTheDocument()
    expect(within(items[2]).getByText('First draft')).toBeInTheDocument()
  })

  it('marks an agent whose name looks like a person, with the name of its user (0003-5)', async () => {
    // Agent names come from the client: this one calls itself like its user. The server's
    // names are used as they come, and the agent mark and "agent of" still tell it apart.
    const head = db.versions.find((v) => v.id === db.nodes.find((n) => n.id === ids.notes)!.head_version)!
    head.actor = { kind: 'agent', id: ids.agentBackup, display_name: 'Ada Lovelace' }
    head.on_behalf_of = ids.ada
    head.on_behalf_of_display_name = 'Ada Lovelace'
    const { panel } = await openVersions()
    const items = await within(panel).findAllByRole('listitem')
    expect(within(items[0]).getByText(/Ada Lovelace \(agent of Ada Lovelace\)/)).toBeInTheDocument()
    expect(within(items[0]).getByText('(an agent)')).toBeInTheDocument()
  })

  it('compares an old version with the current one', async () => {
    const { user, panel } = await openVersions()
    const compare = await within(panel).findAllByRole('button', { name: /^Compare version/ })
    // The oldest version (the third row) compared with the current one, in a wide dialog.
    await user.click(compare[2])
    const d = await dialog('Compare versions')
    const diff = await within(d).findByRole('group', { name: 'Changes' })
    // One plain count, not the counts by kind.
    expect(within(diff).getByText('4 changes')).toBeInTheDocument()
    expect(within(diff).getByText('First idea, refined.')).toBeInTheDocument()
  })

  it('shows the words that changed inside a line, and compares two old versions', async () => {
    const seen = recordRequests()
    const { user, panel } = await openVersions()
    const compare = await within(panel).findAllByRole('button', { name: /^Compare version/ })
    await user.click(compare[2])
    const d = await dialog('Compare versions')
    const diff = await within(d).findByRole('group', { name: 'Changes' })
    // The detail of the changed line 3: "idea." became "idea, refined."
    const line3 = within(diff).getByText('Line 3').closest('li')!
    expect(within(line3).getByText('idea.', { selector: 'ul del' })).toBeInTheDocument()
    expect(within(line3).getByText('idea, refined.', { selector: 'ul ins' })).toBeInTheDocument()
    // Plain words, not the plugin ID.
    expect(within(diff).getByText('Compared as text.')).toBeInTheDocument()
    expect(within(d).queryByText(/text@1\.0\.0/)).not.toBeInTheDocument()

    // The first draft compared with the second version (not the current one).
    await user.selectOptions(within(d).getByRole('combobox', { name: 'To' }), '0190a000-0000-7000-8000-000000000602')
    expect(await within(d).findByText('2 changes')).toBeInTheDocument()
    const last = seen.filter((r) => r.path.includes('/diff?')).at(-1)!
    expect(new URL(last.path, 'http://x').searchParams.get('from')).toBe('0190a000-0000-7000-8000-000000000601')
    expect(new URL(last.path, 'http://x').searchParams.get('to')).toBe('0190a000-0000-7000-8000-000000000602')
  })

  it('compares with another diff plugin', async () => {
    const { user, panel } = await openVersions()
    const compare = await within(panel).findAllByRole('button', { name: /^Compare version/ })
    await user.click(compare[1])
    const d = await dialog('Compare versions')
    await within(d).findByRole('group', { name: 'Changes' })
    // "Compare as" waits behind "More options".
    expect(within(d).queryByRole('combobox', { name: 'Compare as' })).not.toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'More options' }))
    await user.selectOptions(await within(d).findByRole('combobox', { name: 'Compare as' }), 'binary')
    expect(await within(d).findByText('Compared as file.')).toBeInTheDocument()
    expect(within(d).getByText('Content')).toBeInTheDocument()
  })

  it('restores an old version as a new current version', async () => {
    const { user, panel } = await openVersions()
    const restore = await within(panel).findAllByRole('button', { name: /^Restore version/ })
    await user.click(restore[1])
    await user.click(within(await dialog(/^Restore the version of /)).getByRole('button', { name: 'Restore' }))
    expect(await screen.findByText(/^Restored\. The version of .* is current again\.$/)).toBeInTheDocument()
    // The restored row says which version it came from.
    expect(await within(panel).findByText(/^Restored from /)).toBeInTheDocument()
    await waitFor(() => expect(within(panel).getAllByRole('listitem')).toHaveLength(4))
    const head = db.versions.find((v) => v.id === db.nodes.find((n) => n.id === ids.notes)!.head_version)
    expect(head?.restored_from).toBe('0190a000-0000-7000-8000-000000000601')
  })

  it('names a version, and says that named versions are kept forever', async () => {
    const { user, panel } = await openVersions()
    // Naming is one action with one name everywhere (journey J4: "Name this version").
    const more = await within(panel).findAllByRole('button', { name: /^More actions for the version of / })
    await user.click(more[0])
    await user.click(within(screen.getByRole('menu')).getByRole('menuitem', { name: 'Name this version' }))
    const d = await dialog('Name this version')
    expect(within(d).getByText(/Named versions are kept forever/)).toBeInTheDocument()
    expect(within(d).getByRole('textbox')).toHaveFocus()
    await user.type(within(d).getByRole('textbox'), 'Shipped{Enter}')
    expect(await within(panel).findByText('Shipped')).toBeInTheDocument()
  })
})
