import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { Actor } from '../../app/names'
import { formatBytes, formatDate, formatShortDate, formatWhen } from '../../components/format'
import {
  Actions,
  Badge,
  Button,
  ConfirmButton,
  DiffChange,
  DiffList,
  DiffView,
  Dialog,
  EmptyState,
  ErrorNote,
  Field,
  List,
  ListRow,
  Loaded,
  Loading,
  LoadMore,
  Row,
  Select,
  Stack,
  Text,
  Toolbar,
  useToast,
} from '../../ui'
import { downloadNode, type Node } from './api'
import { NameDialog } from './dialogs'

/** Version history of a file: download, name, compare, restore (journeys J4). */
export function VersionsPanel(props: { node: Node; onClose: () => void; onRestored: () => void }) {
  const { node } = props
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const [compare, setCompare] = useState<Schemas['Version'] | null>(null)
  const versions = usePagedApi(
    (cursor) =>
      unwrap(client.GET('/v1/nodes/{node_id}/versions', { params: { path: { node_id: node.id }, query: cursor ? { cursor } : {} } })),
    [node.id, node.head_version],
  )
  const canEdit = node.my_role === 'edit' || node.my_role === 'manage'
  const [naming, setNaming] = useState<Schemas['Version'] | null>(null)

  async function saveName(v: Schemas['Version'], label: string) {
    await unwrap(
      client.PUT('/v1/nodes/{node_id}/versions/{version_id}/label', {
        params: { path: { node_id: node.id, version_id: v.id } },
        body: { label: label || null },
      }),
    )
    toast(label ? `Named the version ${label}.` : 'Removed the version name.')
    versions.reload()
  }

  async function restore(v: Schemas['Version']) {
    const before = node.head_version
    await unwrap(
      client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', { params: { path: { node_id: node.id, version_id: v.id } } }),
    )
    setCompare(null)
    props.onRestored()
    toast(`Restored. The version of ${formatShortDate(v.created_at)} is current again.`, {
      undo: before
        ? async () => {
            await unwrap(
              client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', {
                params: { path: { node_id: node.id, version_id: before } },
              }),
            )
            props.onRestored()
            return 'The version you had before is current again.'
          }
        : undefined,
    })
  }

  return (
    <Stack as="section" gap={4} aria-labelledby="versions-title">
      <Toolbar>
        <h2 id="versions-title">Versions of {node.name}</h2>
        <Button size="sm" onClick={props.onClose}>
          Back to details
        </Button>
      </Toolbar>
      <ErrorNote error={error} />
      {naming && (
        <NameDialog
          open
          title="Name this version"
          help="Named versions are kept forever, also when old versions are removed."
          label="Version name"
          placeholder="Sent to client"
          initial={naming.label ?? ''}
          submitLabel={naming.label ? 'Save name' : 'Save name'}
          allowEmpty
          onClose={() => setNaming(null)}
          onSubmit={(label) => saveName(naming, label)}
        />
      )}
      <Loaded state={versions}>
        {(page) => {
          const byId = new Map(page.items.map((v) => [v.id, v]))
          return (
            <>
              <List ordered>
                {page.items.map((v) => {
                  const current = v.id === node.head_version
                  const when = formatDate(v.created_at)
                  const source = v.restored_from ? byId.get(v.restored_from) : undefined
                  return (
                    <ListRow key={v.id} block current={current}>
                      <Row gap={2}>
                        <strong>{versionWhen(v.created_at)}</strong>
                        {current && <Badge tone="primary">Current</Badge>}
                        {v.label && <Badge>{v.label}</Badge>}
                      </Row>
                      <Text as="div" tone="muted" size="sm">
                        <Actor pair={v} /> - {formatBytes(v.size_bytes)}
                      </Text>
                      {v.restored_from && (
                        <Text as="div" tone="muted" size="sm">
                          {source ? `Restored from ${formatShortDate(source.created_at)}` : 'Restored from an older version'}
                        </Text>
                      )}
                      <Actions align="start">
                        <Button
                          size="sm"
                          aria-label={`Download version of ${when}`}
                          onClick={() => downloadNode(node, v.id).catch(setError)}
                        >
                          Download
                        </Button>
                        {page.items.length > 1 && (
                          <Button size="sm" aria-label={`Compare version of ${when}`} onClick={() => setCompare(v)}>
                            Compare
                          </Button>
                        )}
                        {canEdit && (
                          <Button
                            size="sm"
                            aria-label={v.label ? `Rename version of ${when}` : `Keep forever: name the version of ${when}`}
                            onClick={() => setNaming(v)}
                          >
                            {v.label ? 'Rename' : 'Keep forever…'}
                          </Button>
                        )}
                        {canEdit && !current && (
                          <ConfirmButton
                            size="sm"
                            label="Restore"
                            ariaLabel={`Restore version of ${when}`}
                            title={`Restore the version of ${formatShortDate(v.created_at)}?`}
                            message="It becomes the current version. The version you have now stays in Versions."
                            confirmLabel="Restore"
                            onConfirm={() => restore(v)}
                          />
                        )}
                      </Actions>
                    </ListRow>
                  )
                })}
              </List>
              {page.items.length === 1 && !page.nextCursor && (
                <EmptyState title="This is the only version" text="Ruum keeps a new version each time the file changes." />
              )}
              <Text tone="muted" size="sm">
                Named versions are kept forever. Older versions are removed after the time your plan allows.
              </Text>
              {compare && <CompareDialog node={node} from={compare} versions={page.items} onClose={() => setCompare(null)} />}
            </>
          )
        }}
      </Loaded>
      <LoadMore state={versions} />
    </Stack>
  )
}

const kindLabels: Record<Schemas['DiffChangeKind'], string> = {
  insert: 'Added',
  delete: 'Removed',
  modify: 'Changed',
  move: 'Moved',
  format: 'Format',
  replace: 'Replaced',
}

/**
 * When a version was made, always with the time of day, so "yesterday's" version is easy to find:
 * "13 minutes ago (15:13)", "Yesterday at 18:20", "20 Oct 2026, 14:00".
 */
function versionWhen(iso: string): string {
  const rel = formatWhen(iso)
  if (/ago$|^Just now$/.test(rel)) return `${rel} (${new Date(iso).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })})`
  if (rel.includes(' at ')) return rel
  return formatDate(iso)
}

/** "1 change", "3 changes": the plain summary of a comparison (the counts by kind confused people). */
function changeCount(s: { inserted: number; deleted: number; modified: number; moved?: number | null }): string {
  const n = s.inserted + s.deleted + s.modified + (s.moved ?? 0)
  return n === 1 ? '1 change' : `${n} changes`
}

/** Plain names of the diff plugins (ia.md: "Compare as" text, data, image, file). */
function pluginLabel(id: string): string {
  const name = id.split('@')[0]
  const labels: Record<string, string> = { text: 'Text', json: 'Data (JSON)', xml: 'Data (XML)', image: 'Picture', binary: 'File' }
  return labels[name] ?? name[0].toUpperCase() + name.slice(1)
}

/** A change path in plain words: `lines/2` is "Line 2". */
function pathLabel(c: Schemas['DiffChange']): string | null {
  if (c.label) return c.label
  if (!c.path) return null
  const line = /^lines?\/(\d+)$/.exec(c.path)
  if (line) return `Line ${line[1]}`
  return c.path.replace(/\//g, ' > ')
}

/**
 * The changes between two versions (`diffVersions`) in a wide dialog (a full page on the phone),
 * in the one change model of all diff plugins (design 5.4).
 */
function CompareDialog(props: { node: Node; from: Schemas['Version']; versions: Schemas['Version'][]; onClose: () => void }) {
  const { node, from } = props
  const others = props.versions.filter((v) => v.id !== from.id)
  const [to, setTo] = useState(from.id === node.head_version ? (others[0]?.id ?? '') : '')
  const [plugin, setPlugin] = useState('')
  // "Compare as" is for people who know the plugins; it waits behind "More options".
  const [advanced, setAdvanced] = useState(false)
  const plugins = useApi(
    () => unwrap(client.GET('/v1/nodes/{node_id}/diff/plugins', { params: { path: { node_id: node.id } } })),
    [node.id],
  )
  // `to` empty means the current version. When `from` is the current version, compare an older
  // one to it instead (older first).
  const swap = from.id === node.head_version
  const query = swap ? { from: to, to: from.id } : { from: from.id, ...(to ? { to } : {}) }
  const diff = useApi(
    () =>
      query.from
        ? unwrap(
            client.GET('/v1/nodes/{node_id}/diff', {
              params: { path: { node_id: node.id }, query: { ...query, ...(plugin ? { plugin } : {}) } },
            }),
          )
        : Promise.resolve(null),
    [node.id, query.from, query.to, plugin],
  )
  const pluginItems = plugins.data?.items ?? []
  const label = (v: Schemas['Version']) =>
    `${formatShortDate(v.created_at)}${v.id === node.head_version ? ' (current)' : ''}${v.label ? `, ${v.label}` : ''}`

  return (
    <Dialog title="Compare versions" open onClose={props.onClose} size="full">
      <Stack gap={4}>
        <Row gap={4} align="end">
          <Text>
            <Text as="span" tone="muted">
              {swap ? 'To' : 'From'}
            </Text>{' '}
            <strong>{label(from)}</strong>
          </Text>
          <Field label={swap ? 'From' : 'To'}>
            <Select value={to} onChange={(e) => setTo(e.target.value)}>
              {!swap && <option value="">Current version</option>}
              {others
                .filter((v) => swap || v.id !== node.head_version)
                .map((v) => (
                  <option key={v.id} value={v.id}>
                    {label(v)}
                  </option>
                ))}
            </Select>
          </Field>
          {pluginItems.length > 1 && !advanced && (
            <Button variant="link" onClick={() => setAdvanced(true)}>
              More options
            </Button>
          )}
          {pluginItems.length > 1 && advanced && (
            <Field label="Compare as">
              <Select value={plugin} onChange={(e) => setPlugin(e.target.value)}>
                <option value="">Best match ({pluginLabel(pluginItems[0].name)})</option>
                {pluginItems.map((p) => (
                  <option key={p.name} value={p.name}>
                    {pluginLabel(p.name)}
                  </option>
                ))}
              </Select>
            </Field>
          )}
        </Row>
        {diff.loading && !diff.data ? (
          <Loading label="Comparing" />
        ) : (
          <Loaded state={diff}>
            {(d) =>
              !d ? null : (
                <DiffView busy={diff.loading}>
                  {d.changes.length === 0 ? (
                    <p>These versions are the same.</p>
                  ) : (
                    <>
                      <p>{changeCount(d.summary)}</p>
                      <ChangeList changes={d.changes} />
                    </>
                  )}
                  {d.render?.html && (
                    <p>
                      <a href={d.render.html} target="_blank" rel="noopener noreferrer">
                        Open the full view of the changes
                      </a>
                    </p>
                  )}
                  {d.truncated && <Text tone="muted">Only the first changes are shown. Download both versions to see all.</Text>}
                  <Text tone="muted" size="sm">
                    Compared as {pluginLabel(d.plugin).toLowerCase()}.
                  </Text>
                </DiffView>
              )
            }
          </Loaded>
        )}
        <Actions>
          <Button onClick={props.onClose}>Close</Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}

function ChangeList({ changes }: { changes: Schemas['DiffChange'][] }) {
  return (
    <DiffList>
      {changes.map((c, i) => (
        <DiffChange key={`${c.path ?? ''}-${i}`} kind={kindLabels[c.kind]} where={pathLabel(c)} old={c.old} new={c.new} format={c.change}>
          {c.detail && c.detail.length > 0 && <ChangeList changes={c.detail} />}
        </DiffChange>
      ))}
    </DiffList>
  )
}
