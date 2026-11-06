import { useState } from 'react'
import { client, unwrap } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useTenant } from '../../app/tenant'
import { formatDate } from '../../components/format'
import { ConfirmButton, ErrorNote, Loaded, LoadMore, Notice } from '../../components/ui'

/** The trash: restore nodes, delete them for good, or empty the trash. */
export function TrashPage() {
  const { isAdmin } = useTenant()
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const trash = usePagedApi((cursor) => unwrap(client.GET('/v1/trash', { params: { query: cursor ? { cursor } : {} } })), [])

  async function restore(id: string, name: string) {
    setError(undefined)
    try {
      const res = await unwrap(client.POST('/v1/nodes/{node_id}/restore', { params: { path: { node_id: id } }, body: { on_conflict: 'rename' } }))
      setNotice(res.node.name === name ? `${name} restored.` : `${name} restored as ${res.node.name}.`)
      trash.reload()
    } catch (e) {
      setError(e)
    }
  }

  return (
    <div className="page-body">
      <div className="toolbar">
        <h1>Trash</h1>
        {trash.data && trash.data.items.length > 0 && (
          <ConfirmButton
            label="Empty trash"
            title="Empty the trash?"
            message="Everything in the trash is deleted for good. You cannot undo this."
            confirmLabel="Delete everything"
            danger
            onConfirm={async () => {
              const res = await unwrap(client.POST('/v1/trash/empty'))
              setNotice(`Deleted ${res.purged} items for good.`)
              trash.reload()
            }}
          />
        )}
      </div>
      <p className="muted">
        Items stay in the trash for the time your plan allows.{isAdmin ? ' Admins can restore for longer.' : ''}
      </p>
      <ErrorNote error={error} />
      <Notice text={notice} />
      <Loaded state={trash}>
        {({ items }) =>
          items.length === 0 ? (
            <p className="empty">The trash is empty.</p>
          ) : (
            <table className="table">
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col" className="hide-narrow">
                    Deleted
                  </th>
                  <th scope="col">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.map((n) => (
                  <tr key={n.id}>
                    <th scope="row">{n.name}</th>
                    <td className="hide-narrow">{formatDate(n.trashed_at)}</td>
                    <td className="row-actions">
                      <button type="button" aria-label={`Restore ${n.name}`} onClick={() => restore(n.id, n.name)}>
                        Restore
                      </button>
                      <ConfirmButton
                        label="Delete"
                        ariaLabel={`Delete ${n.name} for good`}
                        title={`Delete ${n.name} for good?`}
                        message="You cannot undo this."
                        confirmLabel="Delete for good"
                        danger
                        onConfirm={async () => {
                          await unwrap(client.DELETE('/v1/nodes/{node_id}', { params: { path: { node_id: n.id } } }))
                          setNotice(`${n.name} deleted for good.`)
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
