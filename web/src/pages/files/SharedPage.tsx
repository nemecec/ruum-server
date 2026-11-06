import { useState } from 'react'
import { Link } from 'react-router'
import type { Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { actorLabel, useNames } from '../../app/useNames'
import { useSession } from '../../app/useSession'
import { formatDate, formatWhen, roleLabels } from '../../components/format'
import { EmptyState, ErrorNote, FileIcon, FolderIcon, Loaded, LoadMore } from '../../components/ui'
import { downloadNode, listSharedWithMe, sharedItemPath } from './api'

/**
 * "Shared with me": the items that other people shared with the person, across all spaces where
 * they are a member or guest. Each item belongs to its own space (`node.tenant_id`): its link and
 * its download use that space, not the space of the page.
 */
export function SharedPage() {
  const shared = usePagedApi(listSharedWithMe, [])
  return (
    <div className="page-body">
      <h1>Shared with me</h1>
      <Loaded state={shared}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title="Nothing shared with you yet" text="When someone shares a file or folder with you, it shows up here." />
          ) : (
            <SharedTable items={items} showSpace />
          )
        }
      </Loaded>
      <LoadMore state={shared} />
    </div>
  )
}

/** A table of shared items. Every item links to a page that works with the person's grant. */
export function SharedTable({ items, showSpace }: { items: Schemas['SharedItem'][]; showSpace?: boolean }) {
  const { tenants } = useSession()
  const names = useNames()
  const [error, setError] = useState<unknown>()
  const spaceName = (id: string) => tenants.find((t) => t.tenant.id === id)?.tenant.name ?? 'A shared space'
  return (
    <>
      <ErrorNote error={error} />
      <table className="table">
        <caption className="sr-only">Shared items</caption>
        <thead>
          <tr>
            <th scope="col">Name</th>
            {showSpace && <th scope="col">Space</th>}
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
            const by = actorLabel(names, shared_by.actor, shared_by.on_behalf_of)
            const sharer = by.text === 'Someone' ? `Someone in ${spaceName(node.tenant_id)}` : by.text
            return (
              <tr key={node.id}>
                <th scope="row">
                  <span className="name-cell">
                    <span className={`kind kind-${node.kind}`}>{node.kind === 'folder' ? <FolderIcon /> : <FileIcon />}</span>
                    <Link to={sharedItemPath(node)}>{node.name}</Link>
                  </span>
                </th>
                {showSpace && <td>{spaceName(node.tenant_id)}</td>}
                <td>{roleLabels[role]}</td>
                <td className="hide-narrow">{sharer}</td>
                <td className="hide-narrow">
                  <span title={formatDate(shared_at)}>{formatWhen(shared_at)}</span>
                </td>
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
