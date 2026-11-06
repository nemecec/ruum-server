import { useEffect, useRef, useState } from 'react'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { actorText, formatBytes, formatDate, roleLabels } from '../../components/format'
import { ConfirmButton, ErrorNote, Loaded } from '../../components/ui'
import { downloadNode, type Node } from './api'
import { MoveDialog, NameDialog } from './dialogs'
import { ShareDialog } from './ShareDialog'

/** The details of a node and its actions, shown beside the file list. */
export function NodeDetails(props: {
  node: Node
  onChanged: () => void
  onGone: (message: string) => void
  onShowVersions: () => void
  onClose: () => void
}) {
  const { node } = props
  const [dialog, setDialog] = useState<'rename' | 'move' | 'share' | null>(null)
  const [error, setError] = useState<unknown>()
  const canEdit = node.my_role === 'edit' || node.my_role === 'manage'
  const heading = useRef<HTMLHeadingElement>(null)

  // Move the focus to the panel when it opens, so keyboard and phone users land on it.
  useEffect(() => {
    heading.current?.focus()
    heading.current?.scrollIntoView?.({ block: 'nearest' })
  }, [node.id])

  return (
    <section className="panel" aria-labelledby="details-title">
      <div className="panel-head">
        <h2 id="details-title" ref={heading} tabIndex={-1}>
          {node.name}
        </h2>
        <button type="button" className="icon" aria-label="Close details" onClick={props.onClose}>
          ×
        </button>
      </div>
      <dl className="facts">
        <dt>Kind</dt>
        <dd>{node.kind === 'folder' ? 'Folder' : node.type ?? 'File'}</dd>
        {node.kind === 'file' && (
          <>
            <dt>Size</dt>
            <dd>{formatBytes(node.size_bytes)}</dd>
          </>
        )}
        <dt>Changed</dt>
        <dd>
          {formatDate(node.updated_at)} by {actorText(node.updated_by.actor, node.updated_by.on_behalf_of)}
        </dd>
        <dt>Created</dt>
        <dd>
          {formatDate(node.created_at)} by {actorText(node.created_by.actor, node.created_by.on_behalf_of)}
        </dd>
        {node.my_role && (
          <>
            <dt>Your access</dt>
            <dd>{roleLabels[node.my_role]}</dd>
          </>
        )}
      </dl>
      <ErrorNote error={error} />
      <div className="actions left wrap">
        {node.kind === 'file' && (
          <button type="button" className="primary" onClick={() => downloadNode(node).catch(setError)}>
            Download
          </button>
        )}
        {node.kind === 'file' && (
          <button type="button" onClick={props.onShowVersions}>
            Version history
          </button>
        )}
        <button type="button" onClick={() => setDialog('share')}>
          Share
        </button>
        {canEdit && (
          <button type="button" onClick={() => setDialog('rename')}>
            Rename
          </button>
        )}
        {canEdit && (
          <button type="button" onClick={() => setDialog('move')}>
            Move
          </button>
        )}
        {canEdit && (
          <ConfirmButton
            label="Move to trash"
            title={`Move ${node.name} to the trash?`}
            message="You can restore it from the trash."
            confirmLabel="Move to trash"
            danger
            onConfirm={async () => {
              await unwrap(client.POST('/v1/nodes/{node_id}/trash', { params: { path: { node_id: node.id } } }))
              props.onGone(`${node.name} is in the trash.`)
            }}
          />
        )}
      </div>
      <NodeTags node={node} canEdit={canEdit} />

      {dialog === 'rename' && (
        <NameDialog
          open
          title={`Rename ${node.name}`}
          label="New name"
          initial={node.name}
          submitLabel="Rename"
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            await unwrap(client.POST('/v1/nodes/{node_id}/rename', { params: { path: { node_id: node.id } }, body: { name } }))
            props.onChanged()
          }}
        />
      )}
      {dialog === 'move' && (
        <MoveDialog node={node} open onClose={() => setDialog(null)} onMoved={() => props.onGone(`${node.name} moved.`)} />
      )}
      {dialog === 'share' && <ShareDialog node={node} open onClose={() => setDialog(null)} onChanged={props.onChanged} />}
    </section>
  )
}

function NodeTags({ node, canEdit }: { node: Node; canEdit: boolean }) {
  const [error, setError] = useState<unknown>()
  const [pick, setPick] = useState('')
  const [creating, setCreating] = useState(false)
  const nodeTags = useApi(
    () => unwrap(client.GET('/v1/nodes/{node_id}/tags', { params: { path: { node_id: node.id } } })),
    [node.id],
  )
  const allTags = useApi(() => unwrap(client.GET('/v1/tags')), [])

  async function attach(tagId: string) {
    setError(undefined)
    try {
      if (tagId === '__new') {
        setCreating(true)
        return
      }
      await unwrap(client.PUT('/v1/nodes/{node_id}/tags/{tag_id}', { params: { path: { node_id: node.id, tag_id: tagId } } }))
      nodeTags.reload()
    } catch (e) {
      setError(e)
    } finally {
      setPick('')
    }
  }

  async function detach(tagId: string) {
    setError(undefined)
    try {
      await unwrap(client.DELETE('/v1/nodes/{node_id}/tags/{tag_id}', { params: { path: { node_id: node.id, tag_id: tagId } } }))
      nodeTags.reload()
    } catch (e) {
      setError(e)
    }
  }

  return (
    <section aria-labelledby="tags-title">
      <h3 id="tags-title">Tags</h3>
      <ErrorNote error={error} />
      <Loaded state={nodeTags}>
        {({ items }) => (
          <ul className="tags">
            {items.length === 0 && <li className="muted">No tags.</li>}
            {items.map((t) => (
              <li key={t.id} className="tag" style={t.color ? { borderColor: t.color } : undefined}>
                {t.name}
                {canEdit && (
                  <button type="button" className="icon" aria-label={`Remove tag ${t.name}`} onClick={() => detach(t.id)}>
                    ×
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </Loaded>
      {canEdit && allTags.data && (
        <label>
          Add a tag
          <select value={pick} onChange={(e) => attach(e.target.value)}>
            <option value="">Choose a tag</option>
            {allTags.data.items
              .filter((t) => !nodeTags.data?.items.some((x) => x.id === t.id))
              .map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            <option value="__new">New tag...</option>
          </select>
        </label>
      )}
      {creating && (
        <NameDialog
          open
          title="New tag"
          label="Tag name"
          initial=""
          submitLabel="Create and add"
          onClose={() => setCreating(false)}
          onSubmit={async (name) => {
            const res = await unwrap(client.POST('/v1/tags', { body: { name } }))
            allTags.reload()
            await attach(res.tag.id)
          }}
        />
      )}
    </section>
  )
}
