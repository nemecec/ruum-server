import { useState, type FormEvent } from 'react'
import { Link, useLocation, useParams } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useNames } from '../../app/useNames'
import { tenantPath, useTenant } from '../../app/tenant'
import { count, formatDate, formatWhen } from '../../components/format'
import { ScopeFields } from './ScopeFields'
import { accessSummary, agentStatus } from './status'
import {
  Actions,
  AgentName,
  Badge,
  BareCheckbox,
  Button,
  Card,
  Columns,
  ConfirmButton,
  ConfirmDialog,
  Dialog,
  EmptyState,
  ErrorNote,
  Field,
  Grow,
  List,
  ListRow,
  ListRowMain,
  Loaded,
  LoadMore,
  Note,
  PageBody,
  SecretBox,
  Stack,
  Text,
  TextInput,
  Toolbar,
  useToast,
} from '../../ui'

type Entry = Schemas['JournalEntry']
type NodeInfo = { name: string; kind: string; parentId: string | null; tenantId: string }

/** Names of the items in a list of entries, looked up by ID (the events carry IDs). */
function useNodeInfo(ids: string[]) {
  return useApi(async () => {
    const out: Record<string, NodeInfo> = {}
    await Promise.all(
      ids.slice(0, 60).map(async (id) => {
        try {
          const n = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: id } } }))
          out[id] = { name: n.parent_id ? n.name : 'Files', kind: n.kind, parentId: n.parent_id, tenantId: n.tenant_id }
        } catch {
          // Purged, or out of the person's view: the entry keeps the name from its own data.
        }
      }),
    )
    return out
  }, [ids.join(',')])
}

const str = (v: unknown) => (typeof v === 'string' ? v : null)

/** "Created notes.md in Pärna 12" (copy.md 3.5, activity verbs). */
function describe(
  e: Entry | Schemas['UndoItem'],
  info: Record<string, NodeInfo>,
): { verb: string; name: string | null; where: string | null } {
  const data = 'data' in e ? e.data : {}
  const node = e.node_id ? info[e.node_id] : undefined
  const name = node?.name ?? str(data.name) ?? str(data.new_name) ?? null
  const parent = (id: unknown) => (typeof id === 'string' ? (info[id]?.name ?? null) : null)
  switch (e.type) {
    case 'node.created':
      return { verb: 'Created', name, where: parent(data.parent_id) }
    case 'node.renamed':
      return { verb: `Renamed ${str(data.old_name) ?? ''} to`, name: str(data.new_name) ?? name, where: null }
    case 'node.moved':
      return { verb: 'Moved', name, where: parent(data.new_parent_id) }
    case 'node.copied':
      return { verb: 'Copied', name, where: null }
    case 'node.trashed':
      return { verb: 'Moved to the trash:', name, where: null }
    case 'node.restored':
      return { verb: 'Restored from the trash:', name, where: null }
    case 'node.purged':
      return { verb: 'Deleted for good:', name, where: null }
    case 'version.created':
      return { verb: 'Changed (new version)', name, where: null }
    case 'version.restored':
      return { verb: 'Restored a version of', name, where: null }
    case 'tag.attached':
      return { verb: `Added the tag ${str(data.tag_name) ?? ''} to`, name, where: null }
    case 'tag.detached':
      return { verb: `Removed the tag ${str(data.tag_name) ?? ''} from`, name, where: null }
    case 'grant.added':
      return { verb: 'Gave access to', name, where: null }
    case 'grant.removed':
      return { verb: 'Removed access to', name, where: null }
    default:
      return { verb: 'Changed', name, where: null }
  }
}

function describePlain(e: Entry, info: Record<string, NodeInfo>): string {
  const d = describe(e, info)
  return [d.verb, d.name, d.where ? `in ${d.where}` : null].filter(Boolean).join(' ')
}

function DescribeText({ e, info }: { e: Entry | Schemas['UndoItem']; info: Record<string, NodeInfo> }) {
  const { tenant } = useTenant()
  const d = describe(e, info)
  const node = e.node_id ? info[e.node_id] : undefined
  const target =
    e.node_id && node && e.type !== 'node.purged'
      ? node.kind === 'folder'
        ? tenantPath(node.tenantId, 'files', e.node_id)
        : tenantPath(tenant.id, 'item', e.node_id)
      : null
  return (
    <>
      {d.verb} {d.name && (target ? <Link to={target}>{d.name}</Link> : <strong>{d.name}</strong>)}
      {d.where && <> in {d.where}</>}
    </>
  )
}

// Entries that this browser undid. The activity list keeps them (the journal is append-only), so
// the page marks them "Undone" and leaves them out of the next "Undo all". A per-viewer convenience.
const UNDONE = 'ruum.undone'

function undoneSeqs(agentId: string): number[] {
  try {
    return (JSON.parse(localStorage.getItem(UNDONE) ?? '{}') as Record<string, number[]>)[agentId] ?? []
  } catch {
    return []
  }
}

function rememberUndone(agentId: string, seqs: number[]) {
  try {
    const all = JSON.parse(localStorage.getItem(UNDONE) ?? '{}') as Record<string, number[]>
    all[agentId] = [...new Set([...(all[agentId] ?? []), ...seqs])].slice(-2000)
    localStorage.setItem(UNDONE, JSON.stringify(all))
  } catch {
    // Storage can be off; the rows then show without the mark.
  }
}

/** One agent: what it may do, its keys, its activity and "undo its changes" (journeys J5). */
export function AgentPage() {
  const { agentId = '' } = useParams()
  const { tenant } = useTenant()
  const names = useNames()
  const { toast } = useToast()
  const location = useLocation()
  const justCreated = (location.state as { justCreated?: boolean } | null)?.justCreated
  const [error, setError] = useState<unknown>()
  const [pausing, setPausing] = useState(false)
  const agent = useApi(() => unwrap(client.GET('/v1/agents/{agent_id}', { params: { path: { agent_id: agentId } } })), [agentId])

  async function resume(a: Schemas['Agent']) {
    setError(undefined)
    try {
      await unwrap(client.POST('/v1/agents/{agent_id}/enable', { params: { path: { agent_id: a.id } } }))
      toast(`Resumed ${a.name}.`)
      agent.reload()
    } catch (e) {
      setError(e)
    }
  }

  return (
    <PageBody>
      <p>
        <Link to={tenantPath(tenant.id, 'agents')}>Agents</Link>
      </p>
      <Loaded state={agent}>
        {(a) => (
          <>
            <Toolbar>
              <h1>
                <AgentName>{a.name}</AgentName>
              </h1>
              {a.disabled_at ? (
                <Button variant="primary" onClick={() => resume(a)}>
                  Resume
                </Button>
              ) : (
                <Button onClick={() => setPausing(true)}>Pause</Button>
              )}
            </Toolbar>
            <Text tone="muted">
              {a.name} for {names.person(a.owner_id)?.name ?? 'someone'} <Badge>{agentStatus(a)}</Badge>
              {a.description ? ` ${a.description}` : ''}
            </Text>
            {a.disabled_at && <Note>Paused. Its keys and connections don't work until you resume it.</Note>}
            {justCreated && <Note>Agent created. Create a key for it below.</Note>}
            <ErrorNote error={error} />
            <Columns>
              <AccessSection agent={a} onSaved={agent.reload} />
              <KeysSection agent={a} />
            </Columns>
            <ActivitySection agent={a} />
            {pausing && (
              <ConfirmDialog
                title={`Pause ${a.name}?`}
                message="It can't read or change anything until you resume it. Its history stays."
                confirmLabel="Pause"
                onClose={() => setPausing(false)}
                onConfirm={async () => {
                  await unwrap(client.POST('/v1/agents/{agent_id}/disable', { params: { path: { agent_id: a.id } } }))
                  toast(`Paused ${a.name}.`)
                  agent.reload()
                }}
              />
            )}
          </>
        )}
      </Loaded>
    </PageBody>
  )
}

function AccessSection({ agent, onSaved }: { agent: Schemas['Agent']; onSaved: () => void }) {
  const { tenant } = useTenant()
  const { toast } = useToast()
  const [editing, setEditing] = useState(false)
  const [scope, setScope] = useState(agent.scope)
  const [error, setError] = useState<unknown>()
  const folder = useApi(
    () =>
      agent.scope.subtree
        ? unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: agent.scope.subtree } } }))
        : Promise.resolve(null),
    [agent.scope.subtree],
  )
  async function submit(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      await unwrap(client.PATCH('/v1/agents/{agent_id}', { params: { path: { agent_id: agent.id } }, body: { scope } }))
      toast('Saved the access.')
      setEditing(false)
      onSaved()
    } catch (err) {
      setError(err)
    }
  }
  return (
    <Card aria-labelledby="scope-title">
      <Toolbar>
        <h2 id="scope-title">Access</h2>
        {!editing && <Button onClick={() => (setScope(agent.scope), setEditing(true))}>Change access</Button>}
      </Toolbar>
      <p>{accessSummary(agent.scope, folder.data?.name ?? null, tenant.name)}.</p>
      <Text tone="muted" size="sm">
        An agent never gets more access than its person has.
      </Text>
      {editing && (
        <Stack as="form" onSubmit={submit}>
          <ScopeFields tenantId={tenant.id} spaceName={tenant.name} scope={scope} onChange={setScope} idPrefix="edit" />
          <ErrorNote error={error} />
          <Actions>
            <Button onClick={() => setEditing(false)}>Cancel</Button>
            <Button variant="primary" type="submit" disabled={scope.subtree === ''}>
              Save access
            </Button>
          </Actions>
        </Stack>
      )}
    </Card>
  )
}

function KeysSection({ agent }: { agent: Schemas['Agent'] }) {
  const agentId = agent.id
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
        client.POST('/v1/agents/{agent_id}/keys', {
          params: { path: { agent_id: agentId } },
          body: name.trim() ? { name: name.trim() } : {},
        }),
      )
      setSecret(res.secret)
      setName('')
      keys.reload()
    } catch (err) {
      setError(err)
    }
  }

  return (
    <Card aria-labelledby="keys-title">
      <Toolbar>
        <h2 id="keys-title">Keys</h2>
        <Button onClick={() => (setSecret(null), setCreating(true))}>New key</Button>
      </Toolbar>
      <Loaded state={keys}>
        {({ items }) => {
          const live = items.filter((k) => !k.revoked_at)
          return (
            <List>
              {live.length === 0 && (
                <Text as="li" tone="muted">
                  No keys. Apps that connect with sign-in (OAuth) don't need a key.
                </Text>
              )}
              {live.map((k) => (
                <ListRow key={k.id}>
                  <Grow>
                    {k.name || 'Key'} <code>{k.prefix}...</code>
                    <Text as="span" tone="muted" size="sm" block>
                      Ends {formatDate(k.expires_at)}, last used {k.last_used_at ? formatWhen(k.last_used_at).toLowerCase() : 'never'}
                    </Text>
                  </Grow>
                  <ConfirmButton
                    label="Turn off"
                    ariaLabel={`Turn off key ${k.prefix}`}
                    title="Turn off this key?"
                    message="Programs that use it stop working at once."
                    confirmLabel="Turn off key"
                    danger
                    onConfirm={async () => {
                      await unwrap(
                        client.DELETE('/v1/agents/{agent_id}/keys/{key_id}', { params: { path: { agent_id: agentId, key_id: k.id } } }),
                      )
                      keys.reload()
                    }}
                  />
                </ListRow>
              ))}
            </List>
          )
        }}
      </Loaded>
      {creating && (
        <Dialog title={`New key for ${agent.name}`} open onClose={() => setCreating(false)}>
          {secret ? (
            <Stack>
              <SecretBox label="Key" secret={secret} note="Copy the key now. Ruum shows it only once." />
              <Actions>
                <Button variant="primary" onClick={() => setCreating(false)}>
                  Done
                </Button>
              </Actions>
            </Stack>
          ) : (
            <Stack as="form" onSubmit={create}>
              <Field label="Key name (optional)">
                <TextInput value={name} onChange={(e) => setName(e.target.value)} placeholder="laptop" />
              </Field>
              <ErrorNote error={error} />
              <Actions>
                <Button onClick={() => setCreating(false)}>Cancel</Button>
                <Button variant="primary" type="submit">
                  Create key
                </Button>
              </Actions>
            </Stack>
          )}
        </Dialog>
      )}
    </Card>
  )
}

type UndoBody = Schemas['UndoRequest']

const reasons: Record<NonNullable<Schemas['UndoItem']['reason']>, string> = {
  changed_later: 'it was changed again later',
  irreversible: "this can't be undone",
  permission_denied: 'you no longer have access',
}

function ActivitySection({ agent }: { agent: Schemas['Agent'] }) {
  const agentId = agent.id
  const { toast } = useToast()
  const [selected, setSelected] = useState<number[]>([])
  const [undone, setUndone] = useState<number[]>(() => undoneSeqs(agentId))
  // The dry run and the request it answered: the confirmed undo reverts exactly what it listed.
  const [report, setReport] = useState<{ result: Schemas['UndoReport']; request: UndoBody } | null>(null)
  const [error, setError] = useState<unknown>()
  const activity = usePagedApi(
    (cursor) =>
      unwrap(client.GET('/v1/agents/{agent_id}/activity', { params: { path: { agent_id: agentId }, query: cursor ? { cursor } : {} } })),
    [agentId],
  )
  const entries = activity.data?.items ?? []
  const ids = new Set<string>()
  for (const e of entries) {
    if (e.node_id) ids.add(e.node_id)
    for (const k of ['parent_id', 'new_parent_id']) if (typeof e.data[k] === 'string') ids.add(e.data[k] as string)
  }
  const info = useNodeInfo([...ids])
  const open = entries.filter((e) => !undone.includes(e.seq))

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
    const seqs = report.result.reverted.map((r) => r.seq)
    const body: UndoBody =
      seqs.length <= 1000 ? { dry_run: false, seqs } : { dry_run: false, since: report.request.since, until: report.request.until }
    const res = await send(body)
    const done = [...res.reverted.map((r) => r.seq), ...res.skipped.filter((s) => s.reason === 'irreversible').map((s) => s.seq)]
    rememberUndone(agentId, done)
    setUndone(undoneSeqs(agentId))
    setReport(null)
    setSelected([])
    toast(
      `Undid ${count(res.reverted.length, 'change')}.${res.skipped.length ? ` ${res.skipped.length} stayed as ${res.skipped.length === 1 ? 'it was' : 'they were'}.` : ''}`,
    )
    activity.reload()
  }

  const n = report?.result.reverted.length ?? 0
  return (
    <Card aria-labelledby="activity-title">
      <Toolbar>
        <h2 id="activity-title">Activity</h2>
        <Button onClick={dryRun} disabled={selected.length === 0 && open.length === 0}>
          {selected.length ? `Undo ${count(selected.length, 'selected change')}...` : 'Undo all...'}
        </Button>
      </Toolbar>
      <ErrorNote error={error} />
      <Loaded state={activity}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title={`${agent.name} hasn't changed anything yet`} text="Its changes show up here." />
          ) : (
            <List>
              {items.map((e) => {
                const isUndone = undone.includes(e.seq)
                return (
                  <ListRow key={e.seq} undone={isUndone}>
                    <BareCheckbox
                      aria-label={`Select: ${describePlain(e, info.data ?? {})}`}
                      disabled={isUndone}
                      checked={selected.includes(e.seq)}
                      onChange={(ev) => setSelected((s) => (ev.target.checked ? [...s, e.seq] : s.filter((x) => x !== e.seq)))}
                    />
                    <ListRowMain>
                      <DescribeText e={e} info={info.data ?? {}} />
                      {isUndone && <Badge>Undone</Badge>}
                    </ListRowMain>
                    <Text as="span" tone="muted" size="sm" title={formatDate(e.at)}>
                      {formatWhen(e.at)}
                    </Text>
                  </ListRow>
                )
              })}
            </List>
          )
        }
      </Loaded>
      <LoadMore state={activity} />
      {report && (
        <ConfirmDialog
          title={n ? `Undo ${count(n, 'change')} by ${agent.name}?` : 'Nothing to undo'}
          confirmLabel={n ? `Undo ${count(n, 'change')}` : 'Close'}
          danger={n > 0}
          onClose={() => setReport(null)}
          onConfirm={n ? confirm : () => setReport(null)}
          message={
            <Stack>
              {n > 0 && (
                <>
                  <p>These changes will be undone:</p>
                  <ul>
                    {report.result.reverted.map((r) => (
                      <li key={r.seq}>
                        <DescribeText e={r} info={info.data ?? {}} />
                      </li>
                    ))}
                  </ul>
                </>
              )}
              {report.result.skipped.length > 0 && (
                <>
                  <p>These stay as they are:</p>
                  <ul>
                    {report.result.skipped.map((s) => (
                      <li key={s.seq}>
                        <DescribeText e={s} info={info.data ?? {}} />: {s.reason ? reasons[s.reason] : 'skipped'}
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </Stack>
          }
        />
      )}
    </Card>
  )
}
