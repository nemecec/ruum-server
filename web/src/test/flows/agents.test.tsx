import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp } from '../render'
import { recordRequests } from '../requests'

describe('agents', () => {
  it('lists agents with their status', async () => {
    renderApp(`${personal}/agents`)
    const claude = (await screen.findByRole('link', { name: 'Claude Desktop' })).closest('tr')!
    expect(within(claude).getByText(/^Active/)).toBeInTheDocument()
    const backup = screen.getByRole('link', { name: 'Nightly backup' }).closest('tr')!
    expect(within(backup).getByText('Paused')).toBeInTheDocument()
  })

  it('registers an agent with a scope and opens it', async () => {
    const seen = recordRequests()
    const { user, router } = renderApp(`${personal}/agents`)
    // "Connect an agent" opens the page with the MCP address; its second card makes an agent with a key.
    await user.click(await screen.findByRole('button', { name: 'Connect an agent' }))
    expect(await screen.findByRole('heading', { name: 'Connect an agent', level: 1 })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Create an agent with a key' }))
    const d = await dialog('Create an agent with a key')
    expect(within(d).getByRole('textbox', { name: 'Name' })).toHaveFocus()
    await user.type(within(d).getByRole('textbox', { name: 'Name' }), 'Researcher')
    await user.selectOptions(within(d).getByRole('combobox', { name: 'Highest role' }), 'view')

    await user.click(within(d).getByRole('radio', { name: 'Only in a folder' }))
    // No folder yet: the agent cannot be created.
    expect(within(d).getByRole('button', { name: 'Create agent' })).toBeDisabled()
    await user.click(within(d).getByRole('button', { name: 'Choose folder' }))
    const picker = await dialog('Choose a folder for the agent')
    await user.click(await within(picker).findByRole('button', { name: 'Documents' }))
    await user.click(await within(picker).findByRole('button', { name: 'Use this folder' }))
    expect(await within(d).findByText('Folder Documents')).toBeInTheDocument()

    await user.click(within(d).getByRole('button', { name: 'Create agent' }))
    expect(await screen.findByRole('heading', { name: 'Researcher', level: 1 })).toBeInTheDocument()
    expect(screen.getByText('Agent created. Now create a key for it below, so it can get to work.')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === '/v1/agents')
    expect(post?.body).toEqual({ name: 'Researcher', scope: { subtree: ids.documents, max_role: 'view', expires_at: null } })
    expect(router.state.location.pathname).toMatch(new RegExp(`^${personal}/agents/`))
  })

  it('shows the plan limit when registering fails', async () => {
    db.limits[ids.personal].limits.agents = 2
    const { user } = renderApp(`${personal}/agents`)
    await user.click(await screen.findByRole('button', { name: 'Connect an agent' }))
    await user.click(await screen.findByRole('button', { name: 'Create an agent with a key' }))
    const d = await dialog('Create an agent with a key')
    await user.type(within(d).getByRole('textbox', { name: 'Name' }), 'One too many{Enter}')
    expect(await within(d).findByRole('alert')).toHaveTextContent("You've reached a limit of your plan. An admin can change the plan.")
  })

  it('creates a key and shows the secret once, then revokes a key', async () => {
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const keys = await screen.findByRole('region', { name: 'Keys' })
    await user.click(within(keys).getByRole('button', { name: 'New key' }))
    const d = await dialog('New key for Claude Desktop')
    await user.type(within(d).getByRole('textbox', { name: 'Key name (optional)' }), 'ci')
    await user.click(within(d).getByRole('button', { name: 'Create key' }))
    const secret = await within(d).findByRole('textbox', { name: 'Key' })
    expect((secret as HTMLInputElement).value).toMatch(/^ruum_agent_[A-Za-z0-9_-]{43}$/)
    await user.click(within(d).getByRole('button', { name: 'Done' }))
    expect(await within(keys).findByText('ci')).toBeInTheDocument()

    await user.click(within(keys).getByRole('button', { name: 'Turn off key ruum_agent_7f3a' }))
    await user.click(within(await dialog('Turn off this key?')).getByRole('button', { name: 'Turn off key' }))
    await waitFor(() => expect(within(keys).queryByText('laptop')).not.toBeInTheDocument())
  })

  it('pauses an agent after a confirmation, and resumes it', async () => {
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    await user.click(await screen.findByRole('button', { name: 'Pause' }))
    const d = await dialog('Pause Claude Desktop?')
    expect(within(d).getByText("It can't read or change anything until you resume it. Its history stays.")).toBeInTheDocument()
    // The least destructive button has the focus.
    expect(within(d).getByRole('button', { name: 'Cancel' })).toHaveFocus()
    await user.click(within(d).getByRole('button', { name: 'Pause' }))
    expect(await screen.findByText('Paused Claude Desktop.')).toBeInTheDocument()
    expect(screen.getByText("Paused. Its keys and connections don't work until you resume it.")).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: 'Resume' }))
    expect(await screen.findByText('Claude Desktop is back at work.')).toBeInTheDocument()
  })

  it('undoes selected changes of an agent after a dry run', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const activity = await screen.findByRole('region', { name: 'Activity' })
    // Rows say what changed, with the item's name and a link to it.
    expect(await within(activity).findByRole('link', { name: 'plan.txt' })).toBeInTheDocument()
    await user.click(await within(activity).findByRole('checkbox', { name: 'Select: Renamed todo.txt to plan.txt' }))
    await user.click(within(activity).getByRole('button', { name: 'Undo 1 selected change...' }))
    const d = await dialog('Undo 1 change by Claude Desktop?')
    expect(within(d).getByText('These changes will be undone:')).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Undo 1 change' }))
    expect(await screen.findByText('Undid 1 change.')).toBeInTheDocument()
    const undos = seen.filter((r) => r.path === `/v1/agents/${ids.agentClaude}/undo`).map((r) => r.body)
    expect(undos).toEqual([
      { dry_run: true, seqs: [43] },
      { dry_run: false, seqs: [43] },
    ])
    // The undone row stays in the list, marked, and cannot be picked again.
    expect(await within(activity).findByText('Undone')).toBeInTheDocument()
    expect(within(activity).getByRole('checkbox', { name: 'Select: Renamed todo.txt to plan.txt' })).toBeDisabled()
  })

  it('names who changed an item later, when the undo skips it (C-11)', async () => {
    // Charles renamed plan.txt after the agent did: the undo leaves it as it is and says who.
    db.journal.push({
      tenant_id: ids.personal,
      seq: 50,
      node_id: ids.plan,
      revision: 2,
      type: 'node.renamed',
      actor: { kind: 'user', id: ids.charles, display_name: 'Charles Babbage' },
      on_behalf_of: null,
      at: '2026-11-05T17:00:00Z',
      data: { old_name: 'plan.txt', new_name: 'plan-final.txt' },
    })
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const activity = await screen.findByRole('region', { name: 'Activity' })
    await user.click(await within(activity).findByRole('checkbox', { name: 'Select: Renamed todo.txt to plan.txt' }))
    await user.click(within(activity).getByRole('button', { name: 'Undo 1 selected change...' }))
    const d = await dialog('Nothing to undo')
    expect(within(d).getByText('These stay as they are:')).toBeInTheDocument()
    expect(within(d).getByRole('listitem')).toHaveTextContent('Renamed todo.txt to plan.txt: Charles Babbage changed it again later')
  })

  it('undoes all with a fixed range, and confirms exactly the entries of the dry run', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const activity = await screen.findByRole('region', { name: 'Activity' })
    await within(activity).findByRole('checkbox', { name: 'Select: Renamed todo.txt to plan.txt' })
    await user.click(within(activity).getByRole('button', { name: 'Undo all...' }))
    const d = await dialog('Undo 3 changes by Claude Desktop?')

    // The agent writes again after the dry run; the confirmed undo must not revert that unseen.
    db.journal.push({ ...db.journal[2], seq: 44, at: new Date().toISOString() })
    await user.click(within(d).getByRole('button', { name: 'Undo 3 changes' }))
    expect(await screen.findByText('Undid 3 changes.')).toBeInTheDocument()

    const undos = seen.filter((r) => r.path === `/v1/agents/${ids.agentClaude}/undo`).map((r) => r.body as Record<string, unknown>)
    expect(undos[0]).toMatchObject({ dry_run: true, since: db.agents[0].created_at })
    expect(typeof undos[0].until).toBe('string')
    expect(undos[0].seqs).toBeUndefined()
    expect(undos[1]).toEqual({ dry_run: false, seqs: [41, 42, 43] })
    expect(db.journal.some((j) => j.seq === 44)).toBe(true)
  })
})
