import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { Actor } from '../../app/names'
import { formatBytes, formatDate, formatShortDate, formatWhen } from '../../components/format'
import { useToast } from '../../components/useToast'
import { ConfirmButton, Dialog, EmptyState, ErrorNote, Loaded, Loading, LoadMore } from '../../components/ui'
import { downloadNode, type Node } from './api'
import { NameDialog } from './dialogs'

/** Version history of a file: download, name, compare, restore (journeys J4). */
export function VersionsPanel(props: { node: Node; onClose: () => void; onRestored: () => void }) {
  const { node } = props
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const [compare, setCompare] = useState<Schemas['Version'] | null>(null)
  const versions = usePagedApi(
    (cursor) => unwrap(client.GET('/v1/nodes/{node_id}/versions', { params: { path: { node_id: node.id }, query: cursor ? { cursor } : {} } })),
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
    await unwrap(client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', { params: { path: { node_id: node.id, version_id: v.id } } }))
    setCompare(null)
    props.onRestored()
    toast(`Restored. The version of ${formatShortDate(v.created_at)} is current again.`, {
      undo: before
        ? async () => {
            await unwrap(
              client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', { params: { path: { node_id: node.id, version_id: before } } }),
            )
            props.onRestored()
            return 'The version you had before is current again.'
          }
        : undefined,
    })
  }

  return (
    <section className="panel" aria-labelledby="versions-title">
      <div className="panel-head">
        <h2 id="versions-title">Versions of {node.name}</h2>
        <button type="button" onClick={props.onClose}>
          Back to details
        </button>
      </div>
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
              <ol className="versions">
                {page.items.map((v) => {
                  const current = v.id === node.head_version
                  const when = formatDate(v.created_at)
                  const source = v.restored_from ? byId.get(v.restored_from) : undefined
                  return (
                    <li key={v.id} className={current ? 'version current' : 'version'}>
                      <div>
                        <strong>{formatWhen(v.created_at)}</strong> <span className="muted small">{formatShortDate(v.created_at)}</span>
                        {current && <span className="badge">Current</span>}
                        {v.label && <span className="badge label">{v.label}</span>}
                      </div>
                      <div className="muted small">
                        <Actor pair={v} /> - {formatBytes(v.size_bytes)}
                      </div>
                      {v.restored_from && (
                        <div className="muted small">
                          {source ? `Restored from ${formatShortDate(source.created_at)}` : 'Restored from an older version'}
                        </div>
                      )}
                      <div className="actions left">
                        <button type="button" aria-label={`Download version of ${when}`} onClick={() => downloadNode(node, v.id).catch(setError)}>
                          Download
                        </button>
                        {page.items.length > 1 && (
                          <button type="button" aria-label={`Compare version of ${when}`} onClick={() => setCompare(v)}>
                            Compare
                          </button>
                        )}
                        {canEdit && (
                          <button
                            type="button"
                            aria-label={v.label ? `Rename version of ${when}` : `Keep forever: name the version of ${when}`}
                            onClick={() => setNaming(v)}
                          >
                            {v.label ? 'Rename' : 'Keep forever…'}
                          </button>
                        )}
                        {canEdit && !current && (
                          <ConfirmButton
                            label="Restore"
                            ariaLabel={`Restore version of ${when}`}
                            title={`Restore the version of ${formatShortDate(v.created_at)}?`}
                            message="It becomes the current version. The version you have now stays in Versions."
                            confirmLabel="Restore"
                            onConfirm={() => restore(v)}
                          />
                        )}
                      </div>
                    </li>
                  )
                })}
              </ol>
              {page.items.length === 1 && !page.nextCursor && (
                <EmptyState title="This is the only version" text="Ruum keeps a new version each time the file changes." />
              )}
              <p className="muted small">Named versions are kept forever. Older versions are removed after the time your plan allows.</p>
              {compare && <CompareDialog node={node} from={compare} versions={page.items} onClose={() => setCompare(null)} />}
            </>
          )
        }}
      </Loaded>
      <LoadMore state={versions} />
    </section>
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
        ? unwrap(client.GET('/v1/nodes/{node_id}/diff', { params: { path: { node_id: node.id }, query: { ...query, ...(plugin ? { plugin } : {}) } } }))
        : Promise.resolve(null),
    [node.id, query.from, query.to, plugin],
  )
  const pluginItems = plugins.data?.items ?? []
  const label = (v: Schemas['Version']) => `${formatShortDate(v.created_at)}${v.id === node.head_version ? ' (current)' : ''}${v.label ? `, ${v.label}` : ''}`

  return (
    <Dialog title="Compare versions" open onClose={props.onClose} full>
      <div className="stack">
        <div className="diff-controls">
          <p>
            <span className="muted">{swap ? 'To' : 'From'}</span> <strong>{label(from)}</strong>
          </p>
          <label>
            {swap ? 'From' : 'To'}
            <select value={to} onChange={(e) => setTo(e.target.value)}>
              {!swap && <option value="">Current version</option>}
              {others
                .filter((v) => swap || v.id !== node.head_version)
                .map((v) => (
                  <option key={v.id} value={v.id}>
                    {label(v)}
                  </option>
                ))}
            </select>
          </label>
          {pluginItems.length > 1 && !advanced && (
            <button type="button" className="link small" onClick={() => setAdvanced(true)}>
              More options
            </button>
          )}
          {pluginItems.length > 1 && advanced && (
            <label>
              Compare as
              <select value={plugin} onChange={(e) => setPlugin(e.target.value)}>
                <option value="">Best match ({pluginLabel(pluginItems[0].name)})</option>
                {pluginItems.map((p) => (
                  <option key={p.name} value={p.name}>
                    {pluginLabel(p.name)}
                  </option>
                ))}
              </select>
            </label>
          )}
        </div>
        {diff.loading && !diff.data ? (
          <Loading label="Comparing" />
        ) : (
          <Loaded state={diff}>
            {(d) =>
              !d ? null : (
                <div className="diff" aria-label="Changes" role="group" aria-busy={diff.loading}>
                  {d.changes.length === 0 ? (
                    <p>These versions are the same.</p>
                  ) : (
                    <>
                      <p>
                        {changeCount(d.summary)}
                      </p>
                      <ChangeList changes={d.changes} />
                    </>
                  )}
                  {d.render?.html && (
                    <p className="small">
                      <a href={d.render.html} target="_blank" rel="noopener noreferrer">
                        Open the full view of the changes
                      </a>
                    </p>
                  )}
                  {d.truncated && <p className="muted">Only the first changes are shown. Download both versions to see all.</p>}
                  <p className="muted small">Compared as {pluginLabel(d.plugin).toLowerCase()}.</p>
                </div>
              )
            }
          </Loaded>
        )}
        <div className="actions">
          <button type="button" onClick={props.onClose}>
            Close
          </button>
        </div>
      </div>
    </Dialog>
  )
}

function ChangeList({ changes }: { changes: Schemas['DiffChange'][] }) {
  return (
    <ul className="diff-changes">
      {changes.map((c, i) => {
        const where = pathLabel(c)
        return (
          <li key={`${c.path ?? ''}-${i}`} className={`diff-${c.kind}`}>
            <span className="diff-kind">{kindLabels[c.kind]}</span>
            {where && <span className="muted"> {where}</span>}
            {(c.old !== undefined || c.new !== undefined || c.change) && (
              <div className="diff-values">
                {c.old !== undefined && (
                  <del>
                    <span className="sr-only">Removed: </span>
                    {c.old}
                  </del>
                )}
                {c.new !== undefined && (
                  <ins>
                    <span className="sr-only">Added: </span>
                    {c.new}
                  </ins>
                )}
                {c.change && <span className="diff-format">{c.change}</span>}
              </div>
            )}
            {c.detail && c.detail.length > 0 && <ChangeList changes={c.detail} />}
          </li>
        )
      })}
    </ul>
  )
}
