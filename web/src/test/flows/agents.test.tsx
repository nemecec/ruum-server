import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, ids, personal, renderApp } from '../render'
import { recordRequests } from '../requests'

describe('agents', () => {
  it('lists agents with their status', async () => {
    renderApp(`${personal}/agents`)
    const claude = (await screen.findByRole('link', { name: 'Claude Desktop' })).closest('tr')!
    expect(within(claude).getByText('Active')).toBeInTheDocument()
    const backup = screen.getByRole('link', { name: 'Nightly backup' }).closest('tr')!
    expect(within(backup).getByText('Disabled')).toBeInTheDocument()
  })

  it('registers an agent with a scope and opens it', async () => {
    const seen = recordRequests()
    const { user, router } = renderApp(`${personal}/agents`)
    await user.click(await screen.findByRole('button', { name: 'Register agent' }))
    const d = await dialog('Register an agent')
    await user.type(within(d).getByRole('textbox', { name: 'Name' }), 'Researcher')
    await user.selectOptions(within(d).getByRole('combobox', { name: 'Highest role' }), 'view')

    await user.click(within(d).getByRole('button', { name: 'Choose folder' }))
    const picker = await dialog('Choose a folder for the agent')
    await user.click(await within(picker).findByRole('button', { name: 'Documents/' }))
    await user.click(await within(picker).findByRole('button', { name: 'Use this folder' }))
    expect(await within(d).findByText('Folder Documents')).toBeInTheDocument()

    await user.click(within(d).getByRole('button', { name: 'Register' }))
    expect(await screen.findByRole('heading', { name: 'Researcher', level: 1 })).toBeInTheDocument()
    expect(screen.getByText('Agent registered. Create a key for it below.')).toBeInTheDocument()
    const post = seen.find((r) => r.method === 'POST' && r.path === '/v1/agents')
    expect(post?.body).toEqual({ name: 'Researcher', scope: { subtree: ids.documents, max_role: 'view', expires_at: null } })
    expect(router.state.location.pathname).toMatch(new RegExp(`^${personal}/agents/`))
  })

  it('shows the plan limit when registering fails', async () => {
    db.limits[ids.personal].limits.agents = 2
    const { user } = renderApp(`${personal}/agents`)
    await user.click(await screen.findByRole('button', { name: 'Register agent' }))
    const d = await dialog('Register an agent')
    await user.type(within(d).getByRole('textbox', { name: 'Name' }), 'One too many{Enter}')
    expect(await within(d).findByRole('alert')).toHaveTextContent('Your plan allows 2 agents.')
  })

  it('creates a key and shows the secret once, then revokes a key', async () => {
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const keys = await screen.findByRole('region', { name: 'API keys' })
    await user.click(within(keys).getByRole('button', { name: 'New key' }))
    const d = await dialog('New API key')
    await user.type(within(d).getByRole('textbox', { name: 'Name (optional)' }), 'ci')
    await user.click(within(d).getByRole('button', { name: 'Create key' }))
    const secret = await within(d).findByRole('textbox', { name: 'API key' })
    expect((secret as HTMLInputElement).value).toMatch(/^ruum_agent_[A-Za-z0-9_-]{43}$/)
    await user.click(within(d).getByRole('button', { name: 'Done' }))
    expect(await within(keys).findByText('ci')).toBeInTheDocument()

    await user.click(within(keys).getByRole('button', { name: 'Revoke key ruum_agent_7f3a' }))
    await user.click(within(await dialog('Revoke this key?')).getByRole('button', { name: 'Revoke key' }))
    await waitFor(() => expect(within(keys).queryByText('laptop')).not.toBeInTheDocument())
  })

  it('disables and enables an agent', async () => {
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    await user.click(await screen.findByRole('button', { name: 'Disable' }))
    expect(await screen.findByText('Agent disabled. Its keys and tokens stop working at once.')).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: 'Enable' }))
    expect(await screen.findByText('Agent enabled.')).toBeInTheDocument()
  })

  it('undoes selected changes of an agent after a dry run', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const activity = await screen.findByRole('region', { name: 'Activity' })
    await user.click(await within(activity).findByRole('checkbox', { name: 'Renamed: plan.txt' }))
    await user.click(within(activity).getByRole('button', { name: 'Undo 1 selected...' }))
    const d = await dialog('Undo changes?')
    expect(within(d).getByText('1 change can be undone.')).toBeInTheDocument()
    await user.click(within(d).getByRole('button', { name: 'Undo 1 change' }))
    expect(await within(activity).findByText('Undid 1 change.')).toBeInTheDocument()
    const undos = seen.filter((r) => r.path === `/v1/agents/${ids.agentClaude}/undo`).map((r) => r.body)
    expect(undos).toEqual([
      { dry_run: true, seqs: [43] },
      { dry_run: false, seqs: [43] },
    ])
    expect(within(activity).queryByText('Renamed: plan.txt')).not.toBeInTheDocument()
  })

  it('undoes all with a fixed range, and confirms exactly the entries of the dry run', async () => {
    const seen = recordRequests()
    const { user } = renderApp(`${personal}/agents/${ids.agentClaude}`)
    const activity = await screen.findByRole('region', { name: 'Activity' })
    await within(activity).findByRole('checkbox', { name: 'Renamed: plan.txt' })
    await user.click(within(activity).getByRole('button', { name: 'Undo all...' }))
    const d = await dialog('Undo changes?')
    expect(within(d).getByText('3 changes can be undone.')).toBeInTheDocument()

    // The agent writes again after the dry run; the confirmed undo must not revert that unseen.
    db.journal.push({ ...db.journal[2], seq: 44, at: new Date().toISOString() })
    await user.click(within(d).getByRole('button', { name: 'Undo 3 changes' }))
    expect(await within(activity).findByText('Undid 3 changes.')).toBeInTheDocument()

    const undos = seen.filter((r) => r.path === `/v1/agents/${ids.agentClaude}/undo`).map((r) => r.body as Record<string, unknown>)
    expect(undos[0]).toMatchObject({ dry_run: true, since: db.agents[0].created_at })
    expect(typeof undos[0].until).toBe('string')
    expect(undos[0].seqs).toBeUndefined()
    expect(undos[1]).toEqual({ dry_run: false, seqs: [41, 42, 43] })
    expect(db.journal.some((j) => j.seq === 44)).toBe(true)
  })
})
