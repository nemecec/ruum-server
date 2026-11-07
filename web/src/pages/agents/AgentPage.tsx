import { useState, type FormEvent } from 'react'
import { Link, useLocation, useParams } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useNames } from '../../app/useNames'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDate, formatWhen, formatWhenInline } from '../../components/format'
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
import { t, tr, type MessageKey } from '../../i18n'

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
          out[id] = { name: n.parent_id ? n.name : t('shell.nav.files'), kind: n.kind, parentId: n.parent_id, tenantId: n.tenant_id }
        } catch {
          // Purged, or out of the person's view: the entry keeps the name from its own data.
        }
      }),
    )
    return out
  }, [ids.join(',')])
}

const str = (v: unknown) => (typeof v === 'string' ? v : null)

type Described = { key: MessageKey; name: string | null; values: Record<string, string> }

/** "Created notes.md in Pärna 12" (copy.md 3.5, activity verbs): a message with `{name}` for the item. */
function describe(e: Entry | Schemas['UndoItem'], info: Record<string, NodeInfo>): Described {
  const data = 'data' in e ? e.data : {}
  const node = e.node_id ? info[e.node_id] : undefined
  const name = node?.name ?? str(data.name) ?? str(data.new_name) ?? null
  const parent = (id: unknown) => (typeof id === 'string' ? (info[id]?.name ?? null) : null)
  const where = (id: unknown) => {
    const w = parent(id)
    return { hasWhere: w ? 'yes' : 'no', where: w ?? '' }
  }
  switch (e.type) {
    case 'node.created':
      return { key: 'activity.created', name, values: where(data.parent_id) }
    case 'node.renamed':
      return { key: 'activity.renamed', name: str(data.new_name) ?? name, values: { old: str(data.old_name) ?? '' } }
    case 'node.moved':
      return { key: 'activity.moved', name, values: where(data.new_parent_id) }
    case 'node.copied':
      return { key: 'activity.copied', name, values: {} }
    case 'node.trashed':
      return { key: 'activity.trashed', name, values: {} }
    case 'node.restored':
      return { key: 'activity.restored', name, values: {} }
    case 'node.purged':
      return { key: 'activity.purged', name, values: {} }
    case 'version.created':
      return { key: 'activity.versionCreated', name, values: {} }
    case 'version.restored':
      return { key: 'activity.versionRestored', name, values: {} }
    case 'tag.attached':
      return { key: 'activity.tagAttached', name, values: { tag: str(data.tag_name) ?? '' } }
    case 'tag.detached':
      return { key: 'activity.tagDetached', name, values: { tag: str(data.tag_name) ?? '' } }
    case 'grant.added':
      return { key: 'activity.grantAdded', name, values: {} }
    case 'grant.removed':
      return { key: 'activity.grantRemoved', name, values: {} }
    default:
      return { key: 'activity.changed', name, values: {} }
  }
}

function describePlain(e: Entry, info: Record<string, NodeInfo>): string {
  const d = describe(e, info)
  return t(d.key, { ...d.values, name: d.name ?? '' })
    .replace(/\s+/g, ' ')
    .trim()
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
  const name = d.name ? target ? <Link to={target}>{d.name}</Link> : <strong>{d.name}</strong> : ''
  return <>{tr(d.key, { ...d.values, name })}</>
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
      toast(t('agent.resumed', { name: a.name }))
      agent.reload()
    } catch (e) {
      setError(e)
    }
  }

  return (
    <PageBody>
      <p>
        <Link to={tenantPath(tenant.id, 'agents')}>{t('shell.nav.agents')}</Link>
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
                  {t('agent.resume')}
                </Button>
              ) : (
                <Button onClick={() => setPausing(true)}>{t('agent.pause')}</Button>
              )}
            </Toolbar>
            <Text tone="muted">
              {names.person(a.owner_id)?.name ? t('agent.for', { name: a.name, person: names.person(a.owner_id)!.name }) : a.name}{' '}
              <Badge>{agentStatus(a)}</Badge>
              {a.description ? ` ${a.description}` : ''}
            </Text>
            {a.disabled_at && <Note>{t('agent.pausedNote')}</Note>}
            {justCreated && <Note>{t('agent.created')}</Note>}
            <ErrorNote error={error} />
            <Columns>
              <AccessSection agent={a} onSaved={agent.reload} />
              <KeysSection agent={a} />
            </Columns>
            <ActivitySection agent={a} />
            {pausing && (
              <ConfirmDialog
                title={t('agent.pause.title', { name: a.name })}
                message={t('agent.pause.message')}
                confirmLabel={t('agent.pause')}
                onClose={() => setPausing(false)}
                onConfirm={async () => {
                  await unwrap(client.POST('/v1/agents/{agent_id}/disable', { params: { path: { agent_id: a.id } } }))
                  toast(t('agent.paused', { name: a.name }))
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
      toast(t('agent.access.saved'))
      setEditing(false)
      onSaved()
    } catch (err) {
      setError(err)
    }
  }
  return (
    <Card aria-labelledby="scope-title">
      <Toolbar>
        <h2 id="scope-title">{t('share.access')}</h2>
        {!editing && <Button onClick={() => (setScope(agent.scope), setEditing(true))}>{t('agent.access.change')}</Button>}
      </Toolbar>
      <p>{t('consent.summary', { summary: accessSummary(agent.scope, folder.data?.name ?? null, tenant.name) })}</p>
      <Text tone="muted" size="sm">
        {t('agent.access.never')}
      </Text>
      {editing && (
        <Stack as="form" onSubmit={submit}>
          <ScopeFields tenantId={tenant.id} spaceName={tenant.name} scope={scope} onChange={setScope} idPrefix="edit" />
          <ErrorNote error={error} />
          <Actions>
            <Button onClick={() => setEditing(false)}>{t('common.cancel')}</Button>
            <Button variant="primary" type="submit" disabled={scope.subtree === ''}>
              {t('agent.access.save')}
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
        <h2 id="keys-title">{t('agent.keys')}</h2>
        <Button onClick={() => (setSecret(null), setCreating(true))}>{t('agent.keys.new')}</Button>
      </Toolbar>
      <Loaded state={keys}>
        {({ items }) => {
          const live = items.filter((k) => !k.revoked_at)
          return (
            <List>
              {live.length === 0 && (
                <Text as="li" tone="muted">
                  {t('agent.keys.none')}
                </Text>
              )}
              {live.map((k) => (
                <ListRow key={k.id}>
                  <Grow>
                    {k.name || t('agent.key')} <code>{k.prefix}...</code>
                    <Text as="span" tone="muted" size="sm" block>
                      {k.last_used_at
                        ? t('agent.key.facts', { ends: formatDate(k.expires_at), used: formatWhenInline(k.last_used_at) })
                        : t('agent.key.facts.unused', { ends: formatDate(k.expires_at) })}
                    </Text>
                  </Grow>
                  <ConfirmButton
                    label={t('account.appPassword.off')}
                    ariaLabel={t('agent.key.off.label', { prefix: k.prefix })}
                    title={t('agent.key.off.title')}
                    message={t('agent.key.off.message')}
                    confirmLabel={t('agent.key.off.confirm')}
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
        <Dialog title={t('agent.key.new.title', { name: agent.name })} open onClose={() => setCreating(false)}>
          {secret ? (
            <Stack>
              <SecretBox label={t('agent.key')} secret={secret} note={t('agent.key.note')} />
              <Actions>
                <Button variant="primary" onClick={() => setCreating(false)}>
                  {t('common.done')}
                </Button>
              </Actions>
            </Stack>
          ) : (
            <Stack as="form" onSubmit={create}>
              <Field label={t('agent.key.name')}>
                <TextInput value={name} onChange={(e) => setName(e.target.value)} placeholder={t('agent.key.placeholder')} />
              </Field>
              <ErrorNote error={error} />
              <Actions>
                <Button onClick={() => setCreating(false)}>{t('common.cancel')}</Button>
                <Button variant="primary" type="submit">
                  {t('agent.key.create')}
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

const reasons: Record<NonNullable<Schemas['UndoItem']['reason']>, MessageKey> = {
  changed_later: 'undo.reason.changedLater',
  irreversible: 'undo.reason.irreversible',
  permission_denied: 'undo.reason.permission',
}

/** How far apart two changes may be and still count as one run of the agent (journey J5). */
const RUN_GAP_MS = 15 * 60 * 1000

/**
 * The entries in runs: changes of the agent that follow each other with less than 15 minutes
 * between them. The entries come newest first; each run too.
 */
function runsOf<T extends { at: string }>(entries: T[]): T[][] {
  const runs: T[][] = []
  for (const e of entries) {
    const run = runs[runs.length - 1]
    const prev = run?.[run.length - 1]
    if (prev && new Date(prev.at).getTime() - new Date(e.at).getTime() < RUN_GAP_MS) run.push(e)
    else runs.push([e])
  }
  return runs
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

  /** The dry run: `seqs` (one run), the selected entries, or everything since the agent was created up to now. */
  async function dryRun(seqs?: number[]) {
    setError(undefined)
    const pick = seqs ?? selected
    const request: UndoBody = pick.length
      ? { dry_run: true, seqs: pick }
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
      res.skipped.length
        ? `${t('undo.done', { n: res.reverted.length })} ${t('undo.done.skipped', { n: res.skipped.length })}`
        : t('undo.done', { n: res.reverted.length }),
    )
    activity.reload()
  }

  // The confirm lists the changes in the order and with the words of the activity list.
  const bySeq = new Map(entries.map((e) => [e.seq, e]))
  const asListed = <T extends { seq: number }>(r: T) => (bySeq.get(r.seq) ?? r) as T
  const inListOrder = <T extends { seq: number }>(list: T[]) => [...list].sort((x, y) => y.seq - x.seq)

  const n = report?.result.reverted.length ?? 0
  return (
    <Card aria-labelledby="activity-title">
      <Toolbar>
        <h2 id="activity-title">{t('activity.title')}</h2>
        <Button onClick={() => dryRun()} disabled={selected.length === 0 && open.length === 0}>
          {selected.length ? t('undo.selected', { n: selected.length }) : t('undo.all')}
        </Button>
      </Toolbar>
      <ErrorNote error={error} />
      <Loaded state={activity}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title={t('activity.none.title', { name: agent.name })} text={t('activity.none.text')} />
          ) : (
            <Stack gap={4}>
              <Text tone="muted" size="sm">
                {t('activity.help')}
              </Text>
              {runsOf(items).map((run) => {
                const openSeqs = run.filter((e) => !undone.includes(e.seq)).map((e) => e.seq)
                const last = run[0]
                return (
                  <Stack as="section" key={last.seq} gap={2} aria-label={t('activity.run', { when: formatWhen(run[run.length - 1].at), n: run.length })}>
                    <Toolbar>
                      <Text as="h3" size="sm" tone="muted">
                        {t('activity.run', { when: formatWhen(run[run.length - 1].at), n: run.length })}
                      </Text>
                      {openSeqs.length > 0 && (
                        <Button size="sm" onClick={() => dryRun(openSeqs)}>
                          {t('activity.run.undo')}
                        </Button>
                      )}
                    </Toolbar>
                    <List>
                      {run.map((e) => {
                        const isUndone = undone.includes(e.seq)
                        return (
                          <ListRow key={e.seq} undone={isUndone}>
                            <BareCheckbox
                              aria-label={t('activity.select', { what: describePlain(e, info.data ?? {}) })}
                              disabled={isUndone}
                              checked={selected.includes(e.seq)}
                              onChange={(ev) => setSelected((s) => (ev.target.checked ? [...s, e.seq] : s.filter((x) => x !== e.seq)))}
                            />
                            <ListRowMain>
                              <DescribeText e={e} info={info.data ?? {}} />
                              {isUndone && <Badge>{t('activity.undone')}</Badge>}
                            </ListRowMain>
                            <Text as="span" tone="muted" size="sm" title={formatDate(e.at)}>
                              {formatWhen(e.at)}
                            </Text>
                          </ListRow>
                        )
                      })}
                    </List>
                  </Stack>
                )
              })}
            </Stack>
          )
        }
      </Loaded>
      <LoadMore state={activity} />
      {report && (
        <ConfirmDialog
          title={n ? t('undo.confirm.title', { n, name: agent.name }) : t('undo.nothing')}
          confirmLabel={n ? t('undo.confirm', { n }) : t('ui.close')}
          danger={n > 0}
          onClose={() => setReport(null)}
          onConfirm={n ? confirm : () => setReport(null)}
          message={
            <Stack>
              {n > 0 && (
                <>
                  <p>{t('undo.list.reverted')}</p>
                  <ul>
                    {inListOrder(report.result.reverted).map((r) => (
                      <li key={r.seq}>
                        <DescribeText e={asListed(r)} info={info.data ?? {}} />
                      </li>
                    ))}
                  </ul>
                </>
              )}
              {report.result.skipped.length > 0 && (
                <>
                  <p>{t('undo.list.skipped')}</p>
                  <ul>
                    {inListOrder(report.result.skipped).map((s) => (
                      <li key={s.seq}>
                        <DescribeText e={asListed(s)} info={info.data ?? {}} />: {s.reason ? t(reasons[s.reason]) : t('undo.reason.skipped')}
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
