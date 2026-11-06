import { useState, type FormEvent } from 'react'
import { Link, useNavigate } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useNames } from '../../app/useNames'
import { tenantPath, useTenant } from '../../app/tenant'
import { roleLabels } from '../../components/format'
import { AgentGlyph, Dialog, EmptyState, ErrorNote, Loaded, LoadMore } from '../../components/ui'
import { ScopeFields } from './ScopeFields'
import { agentStatus } from './status'

/** The agents of the person (all agents of the space for admins). */
export function AgentsPage() {
  const { tenant, isAdmin } = useTenant()
  const names = useNames()
  const navigate = useNavigate()
  const agents = usePagedApi((cursor) => unwrap(client.GET('/v1/agents', { params: { query: cursor ? { cursor } : {} } })), [])
  const connect = () => navigate(tenantPath(tenant.id, 'agents', 'connect'))

  return (
    <div className="page-body">
      <div className="toolbar">
        <h1>Agents</h1>
        <button type="button" className="primary" onClick={connect}>
          Connect an agent
        </button>
      </div>
      <p className="muted">
        Agents work in Ruum for you, with the access you give them. Every change they make shows their name and yours.
        {isAdmin && ` As an admin, you see every agent in ${tenant.name}.`}
      </p>
      <Loaded state={agents}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title="No agents yet" text="Connect Claude, ChatGPT or another AI app, or create an agent with a key for a script.">
              <button type="button" className="primary" onClick={connect}>
                Connect an agent
              </button>
            </EmptyState>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Status</th>
                  <th scope="col" className="hide-narrow">
                    Highest role
                  </th>
                  {isAdmin && (
                    <th scope="col" className="hide-narrow">
                      Works for
                    </th>
                  )}
                </tr>
              </thead>
              <tbody>
                {items.map((a) => (
                  <tr key={a.id}>
                    <th scope="row">
                      <span className="agent-name">
                        <AgentGlyph />
                        <Link to={tenantPath(tenant.id, 'agents', a.id)}>{a.name}</Link>
                      </span>
                    </th>
                    <td>{agentStatus(a)}</td>
                    <td className="hide-narrow">{roleLabels[a.scope.max_role]}</td>
                    {isAdmin && <td className="hide-narrow">{names.person(a.owner_id)?.name ?? 'Someone'}</td>}
                  </tr>
                ))}
              </tbody>
            </table>
          )
        }
      </Loaded>
      <LoadMore state={agents} />
    </div>
  )
}

/** Creates an agent with its own keys (for scripts and the CLI). */
export function RegisterAgentDialog(props: { onClose: () => void; onCreated: (a: Schemas['Agent']) => void }) {
  const { tenant } = useTenant()
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [scope, setScope] = useState<Schemas['AgentScope']>({ subtree: null, max_role: 'edit', expires_at: null })
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(undefined)
    try {
      const agent = await unwrap(
        client.POST('/v1/agents', { body: { name: name.trim(), ...(description.trim() ? { description: description.trim() } : {}), scope } }),
      )
      props.onCreated(agent)
    } catch (err) {
      setError(err)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog title="Create an agent with a key" open onClose={props.onClose} wide>
      <form className="stack" onSubmit={submit}>
        <label>
          Name
          <input required value={name} onChange={(e) => setName(e.target.value)} placeholder="Backup script" />
        </label>
        <label>
          Description (optional)
          <input value={description} onChange={(e) => setDescription(e.target.value)} />
        </label>
        <ScopeFields tenantId={tenant.id} spaceName={tenant.name} scope={scope} onChange={setScope} idPrefix="register" />
        <ErrorNote error={error} />
        <div className="actions">
          <button type="button" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy || !name.trim() || scope.subtree === ''}>
            Create agent
          </button>
        </div>
      </form>
    </Dialog>
  )
}
