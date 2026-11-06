import { useState, type FormEvent } from 'react'
import { Link, useLocation, useParams } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDate } from '../../components/format'
import { ConfirmButton, Dialog, ErrorNote, Loaded, LoadMore, Notice, SecretBox } from '../../components/ui'
import { agentStatus } from './status'
import { ScopeFields } from './ScopeFields'

const eventLabels: Partial<Record<Schemas['EventType'], string>> = {
  'node.created': 'Created',
  'node.renamed': 'Renamed',
  'node.moved': 'Moved',
  'node.copied': 'Copied',
  'node.trashed': 'Moved to trash',
  'node.restored': 'Restored',
  'node.purged': 'Deleted for good',
  'version.created': 'New version',
  'version.restored': 'Restored a version',
  'tag.attached': 'Added a tag',
  'tag.detached': 'Removed a tag',
  'grant.added': 'Shared',
  'grant.removed': 'Removed access',
}

function describe(e: Schemas['JournalEntry']): string {
  const label = eventLabels[e.type] ?? e.type
  const name = typeof e.data.name === 'string' ? e.data.name : typeof e.data.to === 'string' ? e.data.to : null
  return name ? `${label}: ${name}` : label
}

/** One agent: its scope, keys, activity and "undo what this agent did". */
export function AgentPage() {
  const { agentId = '' } = useParams()
  const { tenant } = useTenant()
  const location = useLocation()
  const [notice, setNotice] = useState<string | null>(
    (location.state as { justCreated?: boolean } | null)?.justCreated ? 'Agent registered. Create a key for it below.' : null,
  )
  const [error, setError] = useState<unknown>()
  const agent = useApi(() => unwrap(client.GET('/v1/agents/{agent_id}', { params: { path: { agent_id: agentId } } })), [agentId])

  async function toggle(a: Schemas['Agent']) {
    setError(undefined)
    try {
      if (a.disabled_at) {
        await unwrap(client.POST('/v1/agents/{agent_id}/enable', { params: { path: { agent_id: a.id } } }))
        setNotice('Agent enabled.')
      } else {
        await unwrap(client.POST('/v1/agents/{agent_id}/disable', { params: { path: { agent_id: a.id } } }))
        setNotice('Agent disabled. Its keys and tokens stop working at once.')
      }
      agent.reload()
    } catch (e) {
      setError(e)
    }
  }

  return (
    <div className="page-body">
      <p>
        <Link to={tenantPath(tenant.id, 'agents')}>Back to agents</Link>
      </p>
      <Loaded state={agent}>
        {(a) => (
          <>
            <div className="toolbar">
              <h1>{a.name}</h1>
              <button type="button" className={a.disabled_at ? 'primary' : 'danger'} onClick={() => toggle(a)}>
                {a.disabled_at ? 'Enable' : 'Disable'}
              </button>
            </div>
            <p>
              <span className="badge">{agentStatus(a)}</span> {a.description}
            </p>
            <ErrorNote error={error} />
            <Notice text={notice} />
            <div className="columns">
              <ScopeSection agent={a} onSaved={() => (setNotice('Scope saved.'), agent.reload())} />
              <KeysSection agentId={a.id} />
            </div>
            <ActivitySection agent={a} />
          </>
        )}
      </Loaded>
    </div>
  )
}

function ScopeSection({ agent, onSaved }: { agent: Schemas['Agent']; onSaved: () => void }) {
  const { tenant } = useTenant()
  const [scope, setScope] = useState(agent.scope)
  const [error, setError] = useState<unknown>()
  async function submit(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      await unwrap(client.PATCH('/v1/agents/{agent_id}', { params: { path: { agent_id: agent.id } }, body: { scope } }))
      onSaved()
    } catch (err) {
      setError(err)
    }
  }
  return (
    <section aria-labelledby="scope-title" className="card">
      <h2 id="scope-title">Scope</h2>
      <form className="stack" onSubmit={submit}>
        <ScopeFields tenantId={tenant.id} scope={scope} onChange={setScope} idPrefix="edit" />
        <ErrorNote error={error} />
        <div className="actions">
          <button type="submit" className="primary">
            Save scope
          </button>
        </div>
      </form>
    </section>
  )
}

function KeysSection({ agentId }: { agentId: string }) {
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')
  const [secret, setSecret] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  // listAgentKeys has no paging in the contract.
  const keys = useApi(() => unwrap(client.GET('/v1/agents/{agent_id}/keys', { params: { path: { agent_id: agentId } } })), [agentId])

  async function create(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await unwrap(
        client.POST('/v1/agents/{agent_id}/keys', { params: { path: { agent_id: agentId } }, body: name.trim() ? { name: name.trim() } : {} }),
      )
      setSecret(res.secret)
      setName('')
      keys.reload()
    } catch (err) {
      setError(err)
    }
  }

  return (
    <section aria-labelledby="keys-title" className="card">
      <div className="panel-head">
        <h2 id="keys-title">API keys</h2>
        <button type="button" onClick={() => (setSecret(null), setCreating(true))}>
          New key
        </button>
      </div>
      <Loaded state={keys}>
        {({ items }) => {
          const live = items.filter((k) => !k.revoked_at)
          return (
            <ul className="rows">
              {live.length === 0 && <li className="muted">No keys. Agents that use OAuth do not need one.</li>}
              {live.map((k) => (
                <li key={k.id} className="row">
                  <span className="grow">
                    <code>{k.prefix}...</code> {k.name}
                    <span className="muted small">
                      {' '}
                      - expires {formatDate(k.expires_at)}, last used {k.last_used_at ? formatDate(k.last_used_at) : 'never'}
                    </span>
                  </span>
                  <ConfirmButton
                    label="Revoke"
                    ariaLabel={`Revoke key ${k.prefix}`}
                    title="Revoke this key?"
                    message="Programs that use this key stop working at once."
                    confirmLabel="Revoke key"
                    danger
                    onConfirm={async () => {
                      await unwrap(client.DELETE('/v1/agents/{agent_id}/keys/{key_id}', { params: { path: { agent_id: agentId, key_id: k.id } } }))
                      keys.reload()
                    }}
                  />
                </li>
              ))}
            </ul>
          )
        }}
      </Loaded>
      {creating && (
        <Dialog title="New API key" open onClose={() => setCreating(false)}>
          {secret ? (
            <div className="stack">
              <SecretBox label="API key" secret={secret} />
              <div className="actions">
                <button type="button" className="primary" onClick={() => setCreating(false)}>
                  Done
                </button>
              </div>
            </div>
          ) : (
            <form className="stack" onSubmit={create}>
              <label>
                Name (optional)
                <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="laptop" />
              </label>
              <ErrorNote error={error} />
              <div className="actions">
                <button type="button" onClick={() => setCreating(false)}>
                  Cancel
                </button>
                <button type="submit" className="primary">
                  Create key
                </button>
              </div>
            </form>
          )}
        </Dialog>
      )}
    </section>
  )
}

type UndoBody = Schemas['UndoRequest']

function ActivitySection({ agent }: { agent: Schemas['Agent'] }) {
  const agentId = agent.id
  const [selected, setSelected] = useState<number[]>([])
  // The dry run and the request it answered: the confirmed undo reverts exactly what it listed.
  const [report, setReport] = useState<{ result: Schemas['UndoReport']; request: UndoBody } | null>(null)
  const [error, setError] = useState<unknown>()
  const [notice, setNotice] = useState<string | null>(null)
  const activity = usePagedApi(
    (cursor) =>
      unwrap(client.GET('/v1/agents/{agent_id}/activity', { params: { path: { agent_id: agentId }, query: cursor ? { cursor } : {} } })),
    [agentId],
  )

  async function send(body: UndoBody) {
    return unwrap(client.POST('/v1/agents/{agent_id}/undo', { params: { path: { agent_id: agentId } }, body }))
  }

  /** The dry run: the selected entries, or everything since the agent was created up to now. */
  async function dryRun() {
    setError(undefined)
    const request: UndoBody = selected.length
      ? { dry_run: true, seqs: selected }
      : { dry_run: true, since: agent.created_at, until: new Date().toISOString() }
    try {
      setReport({ result: await send(request), request })
    } catch (e) {
      setError(e)
    }
  }

  /**
   * The confirmed undo sends the entries that the dry run listed, so changes that the agent made
   * after the dry run are not reverted unseen. A very long list falls back to the same fixed range.
   */
  async function confirm() {
    if (!report) return
    setError(undefined)
    const seqs = report.result.reverted.map((r) => r.seq)
    const body: UndoBody =
      seqs.length <= 1000 ? { dry_run: false, seqs } : { dry_run: false, since: report.request.since, until: report.request.until }
    try {
      const res = await send(body)
      setReport(null)
      setSelected([])
      setNotice(`Undid ${res.reverted.length} ${res.reverted.length === 1 ? 'change' : 'changes'}.${res.skipped.length ? ` Skipped ${res.skipped.length}.` : ''}`)
      activity.reload()
    } catch (e) {
      setError(e)
    }
  }

  const reasons: Record<NonNullable<Schemas['UndoItem']['reason']>, string> = {
    changed_later: 'changed later by someone else',
    irreversible: 'cannot be undone',
    permission_denied: 'you lack the access',
  }

  return (
    <section aria-labelledby="activity-title" className="card">
      <div className="panel-head">
        <h2 id="activity-title">Activity</h2>
        <button type="button" onClick={dryRun} disabled={!activity.data?.items.length}>
          {selected.length ? `Undo ${selected.length} selected...` : 'Undo all...'}
        </button>
      </div>
      <ErrorNote error={error} />
      <Notice text={notice} />
      <Loaded state={activity}>
        {({ items }) =>
          items.length === 0 ? (
            <p className="muted">No changes by this agent.</p>
          ) : (
            <ul className="rows">
              {items.map((e) => (
                <li key={e.seq} className="row">
                  <label className="check grow">
                    <input
                      type="checkbox"
                      checked={selected.includes(e.seq)}
                      onChange={(ev) => setSelected((s) => (ev.target.checked ? [...s, e.seq] : s.filter((x) => x !== e.seq)))}
                    />
                    {describe(e)}
                  </label>
                  <span className="muted small">{formatDate(e.at)}</span>
                </li>
              ))}
            </ul>
          )
        }
      </Loaded>
      <LoadMore state={activity} />
      {report && (
        <Dialog title="Undo changes?" open onClose={() => setReport(null)}>
          <div className="stack">
            <p>
              {report.result.reverted.length} {report.result.reverted.length === 1 ? 'change' : 'changes'} can be undone.
            </p>
            {report.result.skipped.length > 0 && (
              <>
                <p>These stay as they are:</p>
                <ul>
                  {report.result.skipped.map((s) => (
                    <li key={s.seq}>
                      {eventLabels[s.type] ?? s.type}: {s.reason ? reasons[s.reason] : 'skipped'}
                    </li>
                  ))}
                </ul>
              </>
            )}
            <div className="actions">
              <button type="button" onClick={() => setReport(null)}>
                Cancel
              </button>
              <button type="button" className="danger" disabled={report.result.reverted.length === 0} onClick={confirm}>
                Undo {report.result.reverted.length} {report.result.reverted.length === 1 ? 'change' : 'changes'}
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </section>
  )
}
