import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { actorText, formatBytes, formatDate } from '../../components/format'
import { ConfirmButton, ErrorNote, Loaded, Loading, LoadMore, Notice } from '../../components/ui'
import { downloadNode, type Node } from './api'
import { NameDialog } from './dialogs'

/** Version history of a file: download, label, compare with the current version, restore. */
export function VersionsPanel(props: { node: Node; onClose: () => void; onRestored: () => void }) {
  const { node } = props
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const [compare, setCompare] = useState<string | null>(null)
  const versions = usePagedApi(
    (cursor) => unwrap(client.GET('/v1/nodes/{node_id}/versions', { params: { path: { node_id: node.id }, query: cursor ? { cursor } : {} } })),
    [node.id, node.head_version],
  )
  const canEdit = node.my_role === 'edit' || node.my_role === 'manage'

  const [labeling, setLabeling] = useState<Schemas['Version'] | null>(null)

  async function saveLabel(v: Schemas['Version'], label: string) {
    await unwrap(
      client.PUT('/v1/nodes/{node_id}/versions/{version_id}/label', {
        params: { path: { node_id: node.id, version_id: v.id } },
        body: { label: label || null },
      }),
    )
    setNotice(label ? 'Label saved.' : 'Label removed.')
    versions.reload()
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
      <Notice text={notice} />
      {labeling && (
        <NameDialog
          open
          title="Label this version"
          label="Label (empty to remove it)"
          initial={labeling.label ?? ''}
          submitLabel="Save label"
          allowEmpty
          onClose={() => setLabeling(null)}
          onSubmit={(label) => saveLabel(labeling, label)}
        />
      )}
      <Loaded state={versions}>
        {(page) => (
          <ol className="versions">
            {page.items.map((v) => {
              const current = v.id === node.head_version
              return (
                <li key={v.id} className={current ? 'version current' : 'version'}>
                  <div>
                    <strong>{formatDate(v.created_at)}</strong>
                    {current && <span className="badge">Current</span>}
                    {v.label && <span className="badge label">{v.label}</span>}
                  </div>
                  <div className="muted small">
                    {actorText(v.actor, v.on_behalf_of)} - {formatBytes(v.size_bytes)}
                    {v.restored_from && ' - restored from an older version'}
                  </div>
                  <div className="actions left">
                    <button type="button" aria-label={`Download version of ${formatDate(v.created_at)}`} onClick={() => downloadNode(node, v.id).catch(setError)}>
                      Download
                    </button>
                    {!current && (
                      <button
                        type="button"
                        aria-pressed={compare === v.id}
                        aria-label={`Compare version of ${formatDate(v.created_at)} with current`}
                        onClick={() => setCompare(compare === v.id ? null : v.id)}
                      >
                        Compare
                      </button>
                    )}
                    {canEdit && (
                      <button type="button" aria-label={`Label version of ${formatDate(v.created_at)}`} onClick={() => setLabeling(v)}>
                        Label
                      </button>
                    )}
                    {canEdit && !current && (
                      <ConfirmButton
                        label="Restore"
                        ariaLabel={`Restore version of ${formatDate(v.created_at)}`}
                        title="Restore this version?"
                        message="Ruum makes a new current version with this content. No version is lost."
                        confirmLabel="Restore"
                        onConfirm={async () => {
                          await unwrap(
                            client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', {
                              params: { path: { node_id: node.id, version_id: v.id } },
                            }),
                          )
                          setCompare(null)
                          setNotice('Version restored.')
                          props.onRestored()
                        }}
                      />
                    )}
                  </div>
                  {compare === v.id && <DiffView nodeId={node.id} from={v.id} />}
                </li>
              )
            })}
          </ol>
        )}
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

function DiffView({ nodeId, from }: { nodeId: string; from: string }) {
  const diff = useApi(
    () => unwrap(client.GET('/v1/nodes/{node_id}/diff', { params: { path: { node_id: nodeId }, query: { from } } })),
    [nodeId, from],
  )
  if (diff.loading && !diff.data) return <Loading label="Comparing" />
  return (
    <Loaded state={diff}>
      {(d) => (
        <div className="diff" aria-label="Changes from this version to the current one">
          <p className="small">
            {d.summary.inserted} added, {d.summary.deleted} removed, {d.summary.modified} changed
            {d.summary.moved ? `, ${d.summary.moved} moved` : ''}
          </p>
          {d.changes.length === 0 && <p className="muted">No changes.</p>}
          <ul>
            {d.changes.map((c, i) => (
              <li key={i} className={`diff-${c.kind}`}>
                <span className="diff-kind">{kindLabels[c.kind]}</span> {c.path && <span className="muted">{c.path}: </span>}
                {c.old !== undefined && <del>{c.old}</del>} {c.new !== undefined && <ins>{c.new}</ins>}
                {c.change && <span>{c.change}</span>}
              </li>
            ))}
          </ul>
          {d.truncated && <p className="muted">The diff is long; only the start is shown.</p>}
        </div>
      )}
    </Loaded>
  )
}
