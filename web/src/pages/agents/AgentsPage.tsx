import { useState, type FormEvent } from 'react'
import { Link, useNavigate } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDate, roleLabels } from '../../components/format'
import { Dialog, ErrorNote, Loaded, LoadMore } from '../../components/ui'
import { ScopeFields } from './ScopeFields'
import { agentStatus } from './status'


/** The agents of the user (all agents of the tenant for admins). */
export function AgentsPage() {
  const { tenant, isAdmin } = useTenant()
  const navigate = useNavigate()
  const [registering, setRegistering] = useState(false)
  const agents = usePagedApi((cursor) => unwrap(client.GET('/v1/agents', { params: { query: cursor ? { cursor } : {} } })), [])

  return (
    <div className="page-body">
      <div className="toolbar">
        <h1>Agents</h1>
        <button type="button" className="primary" onClick={() => setRegistering(true)}>
          Register agent
        </button>
      </div>
      <p className="muted">
        Agents act for you with the access you give them. Every change they make shows their name and yours.
        {isAdmin && ' As an admin, you see all agents of the tenant.'}
      </p>
      <Loaded state={agents}>
        {({ items }) =>
          items.length === 0 ? (
            <p className="empty">No agents yet. Register one, or connect an AI client with OAuth.</p>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col">Status</th>
                  <th scope="col" className="hide-narrow">
                    Highest role
                  </th>
                  <th scope="col" className="hide-narrow">
                    Last used
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.map((a) => (
                  <tr key={a.id}>
                    <th scope="row">
                      <Link to={tenantPath(tenant.id, 'agents', a.id)}>{a.name}</Link>
                    </th>
                    <td>{agentStatus(a)}</td>
                    <td className="hide-narrow">{roleLabels[a.scope.max_role]}</td>
                    <td className="hide-narrow">{a.last_used_at ? formatDate(a.last_used_at) : 'Never'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )
        }
      </Loaded>
      <LoadMore state={agents} />
      {registering && (
        <RegisterAgentDialog
          onClose={() => setRegistering(false)}
          onCreated={(a) => navigate(tenantPath(tenant.id, 'agents', a.id), { state: { justCreated: true } })}
        />
      )}
    </div>
  )
}

function RegisterAgentDialog(props: { onClose: () => void; onCreated: (a: Schemas['Agent']) => void }) {
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
    <Dialog title="Register an agent" open onClose={props.onClose} wide>
      <form className="stack" onSubmit={submit}>
        <label>
          Name
          <input required autoFocus value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label>
          Description (optional)
          <input value={description} onChange={(e) => setDescription(e.target.value)} />
        </label>
        <ScopeFields tenantId={tenant.id} scope={scope} onChange={setScope} idPrefix="register" />
        <ErrorNote error={error} />
        <div className="actions">
          <button type="button" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy || !name.trim()}>
            Register
          </button>
        </div>
      </form>
    </Dialog>
  )
}
