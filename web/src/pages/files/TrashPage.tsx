import { useState } from 'react'
import { client, unwrap } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useTenant } from '../../app/tenant'
import { count, formatDate, formatWhen } from '../../components/format'
import { useToast } from '../../components/useToast'
import { ConfirmButton, EmptyState, ErrorNote, FileIcon, FolderIcon, Loaded, LoadMore } from '../../components/ui'

/** The trash: restore items, delete them for good, or empty the trash (copy.md 3.7). */
export function TrashPage() {
  const { isAdmin, tenant } = useTenant()
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const trash = usePagedApi((cursor) => unwrap(client.GET('/v1/trash', { params: { query: cursor ? { cursor } : {} } })), [])

  async function restore(id: string, name: string) {
    setError(undefined)
    try {
      const res = await unwrap(client.POST('/v1/nodes/{node_id}/restore', { params: { path: { node_id: id } }, body: { on_conflict: 'rename' } }))
      let folder = 'its folder'
      if (res.node.parent_id) {
        try {
          const parent = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: res.node.parent_id } } }))
          folder = parent.parent_id ? parent.name : 'Files'
        } catch {
          // The folder name is only for the toast.
        }
      }
      toast(res.node.name === name ? `Restored ${name} to ${folder}.` : `Restored ${name} to ${folder} as ${res.node.name}.`)
      trash.reload()
    } catch (e) {
      setError(e)
    }
  }

  const n = trash.data?.items.length ?? 0
  return (
    <div className="page-body">
      <div className="toolbar">
        <h1>Trash</h1>
        {n > 0 && (
          <ConfirmButton
            label="Empty trash"
            title={trash.data?.nextCursor ? 'Delete everything in the trash for good?' : `Delete ${count(n, 'item')} for good?`}
            message="Everything in the trash is deleted, with its versions. You can't undo this."
            confirmLabel={trash.data?.nextCursor ? 'Delete everything for good' : `Delete ${count(n, 'item')} for good`}
            danger
            onConfirm={async () => {
              const res = await unwrap(client.POST('/v1/trash/empty'))
              toast(`Deleted ${count(res.purged, 'item')} for good.`)
              trash.reload()
            }}
          />
        )}
      </div>
      <p className="muted">
        Items stay in the trash for the time the plan of {tenant.name} allows. Then Ruum deletes them for good.
        {isAdmin ? ' Admins can restore items for longer.' : ''}
      </p>
      <ErrorNote error={error} />
      <Loaded state={trash}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title="The trash is empty" text="Items you move to the trash stay here for a while, so you can bring them back." />
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col" className="hide-narrow">
                    Moved to the trash
                  </th>
                  <th scope="col">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.map((node) => (
                  <tr key={node.id}>
                    <th scope="row">
                      <span className="name-cell">
                        <span className={`kind kind-${node.kind}`}>{node.kind === 'folder' ? <FolderIcon /> : <FileIcon />}</span>
                        {node.name}
                      </span>
                    </th>
                    <td className="hide-narrow">
                      <span title={formatDate(node.trashed_at)}>{formatWhen(node.trashed_at)}</span>
                    </td>
                    <td className="row-actions">
                      <button type="button" aria-label={`Restore ${node.name}`} onClick={() => restore(node.id, node.name)}>
                        Restore
                      </button>
                      <ConfirmButton
                        label="Delete for good"
                        ariaLabel={`Delete for good: ${node.name}`}
                        title={`Delete ${node.name} for good?`}
                        message="You can't undo this. Its versions are deleted too."
                        confirmLabel="Delete for good"
                        danger
                        onConfirm={async () => {
                          await unwrap(client.DELETE('/v1/nodes/{node_id}', { params: { path: { node_id: node.id } } }))
                          toast(`Deleted ${node.name} for good.`)
                          trash.reload()
                        }}
                      />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )
        }
      </Loaded>
      <LoadMore state={trash} />
    </div>
  )
}
