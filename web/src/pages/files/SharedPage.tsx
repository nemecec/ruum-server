import { useState } from 'react'
import { Link } from 'react-router'
import type { Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { tenantPath } from '../../app/tenant'
import { useSession } from '../../app/useSession'
import { actorText, formatDate, roleLabels } from '../../components/format'
import { ErrorNote, Loaded, LoadMore } from '../../components/ui'
import { downloadNode, listSharedWithMe } from './api'

/**
 * "Shared with me": the nodes that other people shared with the user, across all tenants where the
 * user is a member or guest. Each item belongs to its own tenant (`node.tenant_id`): its link and
 * its download use that tenant, not the tenant of the page.
 */
export function SharedPage() {
  const shared = usePagedApi(listSharedWithMe, [])
  return (
    <div className="page-body">
      <h1>Shared with me</h1>
      <Loaded state={shared}>
        {({ items }) =>
          items.length === 0 ? <p className="empty">Nothing is shared with you yet.</p> : <SharedTable items={items} showTenant />
        }
      </Loaded>
      <LoadMore state={shared} />
    </div>
  )
}

/** A table of shared items. Folders link to the file browser of their own tenant. */
export function SharedTable({ items, showTenant }: { items: Schemas['SharedItem'][]; showTenant?: boolean }) {
  const { tenants } = useSession()
  const [error, setError] = useState<unknown>()
  const tenantName = (id: string) => tenants.find((t) => t.tenant.id === id)?.tenant.name
  return (
    <>
      <ErrorNote error={error} />
      <table className="table">
        <caption className="sr-only">Shared items</caption>
        <thead>
          <tr>
            <th scope="col">Name</th>
            {showTenant && <th scope="col">Tenant</th>}
            <th scope="col">Your access</th>
            <th scope="col" className="hide-narrow">
              Shared by
            </th>
            <th scope="col" className="hide-narrow">
              Shared
            </th>
            <th scope="col">
              <span className="sr-only">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {items.map(({ node, role, shared_by, shared_at }) => {
            // Guest tenants are in the user's tenant list too; a link only works for a listed one.
            const known = tenantName(node.tenant_id) !== undefined
            return (
              <tr key={node.id}>
                <th scope="row">
                  {node.kind === 'folder' && known ? <Link to={tenantPath(node.tenant_id, 'files', node.id)}>{node.name}</Link> : node.name}
                </th>
                {showTenant && <td>{tenantName(node.tenant_id) ?? 'Another tenant'}</td>}
                <td>{roleLabels[role]}</td>
                <td className="hide-narrow">{actorText(shared_by.actor, shared_by.on_behalf_of)}</td>
                <td className="hide-narrow">{formatDate(shared_at)}</td>
                <td className="row-actions">
                  {node.kind === 'file' && (
                    <button
                      type="button"
                      aria-label={`Download ${node.name}`}
                      onClick={() => {
                        setError(undefined)
                        downloadNode(node).catch(setError)
                      }}
                    >
                      Download
                    </button>
                  )}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </>
  )
}
