import { useCallback, useRef, useState, type ChangeEvent } from 'react'
import { Link, useParams, useSearchParams } from 'react-router'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { tenantPath, useTenant } from '../../app/tenant'
import { actorText, formatBytes, formatDate } from '../../components/format'
import { ErrorNote, Loaded, LoadMore, Notice } from '../../components/ui'
import { listChildren, listSharedWithMe, loadFolder, newFileTarget, newVersionTarget, type Node } from './api'
import { NameDialog } from './dialogs'
import { NodeDetails } from './NodeDetails'
import { SharedTable } from './SharedPage'
import { UploadList } from './UploadList'
import { useUploads } from './useUploads'
import { VersionsPanel } from './VersionsPanel'

/** The file browser: one folder, its breadcrumbs, its children and the details of one child. */
export function FilesPage() {
  const { folderId } = useParams()
  const { tenant } = useTenant()
  const [params, setParams] = useSearchParams()
  const selectedId = params.get('node')
  const panel = params.get('panel')
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const [newFolder, setNewFolder] = useState(false)
  const fileInput = useRef<HTMLInputElement>(null)

  const state = useApi(() => loadFolder(folderId), [folderId])
  const loadedId = state.data?.folder?.id
  const children = usePagedApi(
    (cursor) => (loadedId ? listChildren(loadedId, cursor) : Promise.resolve({ items: [] as Node[], next_cursor: null })),
    [loadedId],
  )
  const reload = () => {
    state.reload()
    children.reload()
  }

  const selected = children.data?.items.find((n) => n.id === selectedId) ?? null

  function select(node: Node | null, nextPanel?: string) {
    const next = new URLSearchParams(params)
    if (node) next.set('node', node.id)
    else next.delete('node')
    if (nextPanel) next.set('panel', nextPanel)
    else next.delete('panel')
    setParams(next, { replace: true })
  }

  const reloadFolder = state.reload
  const reloadChildren = children.reload
  const uploads = useUploads(
    useCallback((nodes: Node[], failed: number) => {
      if (nodes.length) {
        const what = nodes.length === 1 ? nodes[0].name : `${nodes.length} files`
        setNotice(failed ? `Uploaded ${what}; ${failed} failed.` : `Uploaded ${what}.`)
      }
      reloadFolder()
      reloadChildren()
    }, [reloadFolder, reloadChildren]),
  )

  function onUpload(e: ChangeEvent<HTMLInputElement>) {
    const files = Array.from(e.target.files ?? [])
    e.target.value = ''
    const folder = state.data?.folder
    if (!files.length || !folder) return
    setError(undefined)
    setNotice(null)
    uploads.add(files, (f) => newFileTarget(folder, f), folder.tenant_id)
  }

  function onUploadVersion(node: Node, file: File) {
    setError(undefined)
    setNotice(null)
    uploads.add([file], () => newVersionTarget(node), node.tenant_id)
  }

  return (
    <div className="files-layout">
      <div className="files-main">
        <Loaded state={state}>
          {({ folder, ancestors }) => {
            if (!folder) return <GuestHome tenantId={tenant.id} />
            const canEdit = folder.my_role === 'edit' || folder.my_role === 'manage'
            return (
              <>
                <nav aria-label="Breadcrumbs" className="breadcrumbs" hidden={ancestors.length < 2}>
                  <ol>
                    {ancestors.map((a, i) => {
                      const label = a.parent_id ? a.name : tenant.name
                      const last = i === ancestors.length - 1
                      return (
                        <li key={a.id}>
                          {last ? (
                            <span aria-current="page">{label}</span>
                          ) : (
                            <Link to={a.parent_id ? tenantPath(tenant.id, 'files', a.id) : tenantPath(tenant.id, 'files')}>{label}</Link>
                          )}
                        </li>
                      )
                    })}
                  </ol>
                </nav>
                <div className="toolbar">
                  <h1>{folder.parent_id ? folder.name : 'Files'}</h1>
                  {canEdit && (
                    <div className="actions">
                      <button type="button" onClick={() => setNewFolder(true)}>
                        New folder
                      </button>
                      <button type="button" className="primary" onClick={() => fileInput.current?.click()}>
                        Upload
                      </button>
                      <input ref={fileInput} type="file" multiple hidden aria-label="Upload files" onChange={onUpload} />
                    </div>
                  )}
                </div>
                <ErrorNote error={error} />
                <Notice text={notice} />
                <UploadList items={uploads.items} onCancel={uploads.cancel} onClear={uploads.clear} />
                <Loaded state={children}>
                  {({ items }) => (
                    <FileTable nodes={items} tenantId={tenant.id} selectedId={selectedId} onSelect={(n) => select(n)} />
                  )}
                </Loaded>
                <LoadMore state={children} />
                {newFolder && (
                  <NameDialog
                    open
                    title="New folder"
                    label="Folder name"
                    initial="New folder"
                    submitLabel="Create"
                    onClose={() => setNewFolder(false)}
                    onSubmit={async (name) => {
                      await unwrap(client.POST('/v1/nodes', { body: { parent_id: folder.id, name, kind: 'folder' } }))
                      setNotice(`Folder ${name} created.`)
                      reload()
                    }}
                  />
                )}
              </>
            )
          }}
        </Loaded>
      </div>
      {selected && (
        <aside className="files-aside" aria-label="Details">
          {panel === 'versions' && selected.kind === 'file' ? (
            <VersionsPanel node={selected} onClose={() => select(selected)} onRestored={reload} />
          ) : (
            <NodeDetails
              key={selected.id}
              node={selected}
              onChanged={reload}
              onGone={(message) => {
                setNotice(message)
                select(null)
                reload()
              }}
              onShowVersions={() => select(selected, 'versions')}
              onUploadVersion={(file) => onUploadVersion(selected, file)}
              onClose={() => select(null)}
            />
          )}
        </aside>
      )}
    </div>
  )
}

/**
 * The files page of a tenant whose root the user cannot see (a guest): the items shared with the
 * user in this tenant.
 */
function GuestHome({ tenantId }: { tenantId: string }) {
  const shared = usePagedApi(listSharedWithMe, [])
  return (
    <>
      <h1>Files</h1>
      <p>You are a guest in this tenant. You see only what other people shared with you.</p>
      <Loaded state={shared}>
        {({ items }) => {
          const here = items.filter((s) => s.node.tenant_id === tenantId)
          return here.length === 0 ? <p className="empty">Nothing here is shared with you yet.</p> : <SharedTable items={here} />
        }}
      </Loaded>
      <LoadMore state={shared} />
    </>
  )
}

function FileTable(props: { nodes: Node[]; tenantId: string; selectedId: string | null; onSelect: (n: Node) => void }) {
  if (props.nodes.length === 0) {
    return <p className="empty">This folder is empty.</p>
  }
  return (
    <table className="table files">
      <caption className="sr-only">Contents of this folder</caption>
      <thead>
        <tr>
          <th scope="col">Name</th>
          <th scope="col" className="hide-narrow">
            Size
          </th>
          <th scope="col" className="hide-narrow">
            Changed
          </th>
          <th scope="col">
            <span className="sr-only">Actions</span>
          </th>
        </tr>
      </thead>
      <tbody>
        {props.nodes.map((n) => (
          <tr key={n.id} className={n.id === props.selectedId ? 'selected' : undefined}>
            <th scope="row">
              <span className="name-cell">
                <span className={`kind kind-${n.kind}`} aria-hidden="true" />
                {n.kind === 'folder' ? (
                  <Link to={tenantPath(props.tenantId, 'files', n.id)}>{n.name}</Link>
                ) : (
                  <button type="button" className="link" onClick={() => props.onSelect(n)}>
                    {n.name}
                  </button>
                )}
              </span>
            </th>
            <td className="hide-narrow">{n.kind === 'folder' ? '-' : formatBytes(n.size_bytes)}</td>
            <td className="hide-narrow" title={actorText(n.updated_by.actor, n.updated_by.on_behalf_of)}>
              {formatDate(n.updated_at)}
            </td>
            <td className="row-actions">
              <button type="button" aria-label={`Details of ${n.name}`} onClick={() => props.onSelect(n)}>
                Details
              </button>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}
