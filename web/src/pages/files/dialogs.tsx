import { useState, type FormEvent } from 'react'
import { client, RuumApiError, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { count } from '../../components/format'
import { Dialog, ErrorNote, FolderIcon, Loaded } from '../../components/ui'
import { isNoAccess, listSharedWithMe, loadAncestors, type Node } from './api'

/** A dialog with one name field, for "New folder", "Rename" and version names. */
export function NameDialog(props: {
  title: string
  label: string
  initial: string
  submitLabel: string
  open: boolean
  onClose: () => void
  onSubmit: (name: string) => Promise<unknown>
  allowEmpty?: boolean
  placeholder?: string
  /** Select the name without its extension when the dialog opens (Rename of a file). */
  selectBase?: boolean
  help?: string
}) {
  const [name, setName] = useState(props.initial)
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(undefined)
    try {
      await props.onSubmit(name.trim())
      props.onClose()
    } catch (err) {
      setError(err)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog title={props.title} open={props.open} onClose={props.onClose}>
      <form className="stack" onSubmit={submit}>
        {props.help && <p className="muted">{props.help}</p>}
        <label>
          {props.label}
          <input
            required={!props.allowEmpty}
            value={name}
            placeholder={props.placeholder}
            data-select-base={props.selectBase ? '' : undefined}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <ErrorNote error={error} context={{ name: name.trim() }} />
        <div className="actions">
          <button type="button" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy || (!props.allowEmpty && !name.trim())}>
            {props.submitLabel}
          </button>
        </div>
      </form>
    </Dialog>
  )
}

/**
 * Browses the folders of a space and returns the picked folder. It starts at `startId`, or at the
 * top. When the person cannot see a folder (a guest, or a folder shared inside a tree), it shows the
 * folders shared with them in this space instead, as the top level.
 */
export function FolderPicker(props: {
  tenantId: string
  startId?: string
  /** Folders that cannot be the target (a folder can't go inside itself). */
  disabledIds?: string[]
  /** The folder the items are in now ("It's already here."). */
  currentId?: string
  onPick: (folder: Node) => Promise<unknown> | void
  pickLabel: string
}) {
  const { tenantId } = props
  // `null` is the top level of a guest: the folders shared with them in this space.
  const [folderId, setFolderId] = useState<string | undefined | null>(props.startId)
  const [creating, setCreating] = useState(false)
  const [busy, setBusy] = useState(false)
  const header = { 'Ruum-Tenant': tenantId }
  const disabled = new Set(props.disabledIds ?? [])
  const state = useApi(async (): Promise<{ folder: Node | null; ancestors: Node[]; folders: Node[] }> => {
    let folder: Node
    try {
      if (folderId === null) throw new RuumApiError(404, { code: 'not_found', message: 'the shared folders' })
      folder = folderId
        ? await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: folderId }, header } }))
        : await unwrap(client.GET('/v1/nodes/root', { params: { header } }))
    } catch (e) {
      if (!isNoAccess(e)) throw e
      // The shared folders of this space are the top level the person can see.
      const shared: Node[] = []
      let cursor: string | undefined
      for (let i = 0; i < 20; i++) {
        const page = await listSharedWithMe(cursor)
        shared.push(...page.items.map((s) => s.node).filter((n) => n.tenant_id === tenantId && n.kind === 'folder'))
        if (!page.next_cursor) break
        cursor = page.next_cursor
      }
      return { folder: null, ancestors: [], folders: shared }
    }
    const folders: Node[] = []
    let cursor: string | undefined
    for (let i = 0; i < 20; i++) {
      const page = await unwrap(
        client.GET('/v1/nodes/{node_id}/children', {
          params: { path: { node_id: folder.id }, query: { kind: 'folder', ...(cursor ? { cursor } : {}) }, header },
        }),
      )
      folders.push(...page.items.filter((n) => n.kind === 'folder'))
      if (!page.next_cursor) break
      cursor = page.next_cursor
    }
    return { folder, ancestors: await loadAncestors(folder), folders }
  }, [folderId, tenantId])

  return (
    <Loaded state={state}>
      {({ folder, ancestors, folders }) => {
        const inside = folder && (disabled.has(folder.id) || ancestors.some((a) => disabled.has(a.id)))
        const here = folder?.id === props.currentId
        const reason = inside ? "A folder can't go inside itself." : here ? "It's already here." : null
        const canCreate = folder && (folder.my_role === 'edit' || folder.my_role === 'manage')
        return (
          <div className="stack">
            <nav aria-label="Folder path" className="breadcrumbs">
              <ol>
                {folder && ancestors[0]?.parent_id && (
                  <li>
                    <button type="button" className="link" onClick={() => setFolderId(null)}>
                      Folders shared with you
                    </button>
                  </li>
                )}
                {folder ? (
                  ancestors.map((a, i) => {
                    const label = a.parent_id ? a.name : 'Files'
                    return (
                      <li key={a.id}>
                        {i === ancestors.length - 1 ? (
                          <span aria-current="location">{label}</span>
                        ) : (
                          <button type="button" className="link" onClick={() => setFolderId(a.id)}>
                            {label}
                          </button>
                        )}
                      </li>
                    )
                  })
                ) : (
                  <li>
                    <span aria-current="location">Folders shared with you</span>
                  </li>
                )}
              </ol>
            </nav>
            <ul className="picker" aria-label="Folders">
              {folders.map((f) => (
                <li key={f.id}>
                  <button type="button" className="picker-item" disabled={disabled.has(f.id)} onClick={() => setFolderId(f.id)}>
                    <FolderIcon />
                    {f.name}
                  </button>
                </li>
              ))}
              {folders.length === 0 && <li className="muted">No folders here.</li>}
            </ul>
            {reason && <p className="muted small">{reason}</p>}
            {folder && (
              <div className="actions">
                {canCreate && (
                  <button type="button" onClick={() => setCreating(true)}>
                    New folder
                  </button>
                )}
                <button
                  type="button"
                  className="primary"
                  disabled={busy || Boolean(reason)}
                  onClick={async () => {
                    setBusy(true)
                    try {
                      await props.onPick(folder)
                    } finally {
                      setBusy(false)
                    }
                  }}
                >
                  {props.pickLabel}
                </button>
              </div>
            )}
            {creating && folder && (
              <NameDialog
                open
                title="New folder"
                label="Folder name"
                initial=""
                submitLabel="Create"
                onClose={() => setCreating(false)}
                onSubmit={async (name) => {
                  const res = await unwrap(client.POST('/v1/nodes', { params: { header }, body: { parent_id: folder.id, name, kind: 'folder' } }))
                  setFolderId(res.node.id)
                }}
              />
            )}
          </div>
        )
      }}
    </Loaded>
  )
}

/** Move one item or several (copy.md 3.2). `onMove` does the move and shows the toast. */
export function MoveDialog(props: { nodes: Node[]; open: boolean; onClose: () => void; onMove: (target: Node) => Promise<void> }) {
  const [error, setError] = useState<unknown>()
  const { nodes } = props
  const first = nodes[0]
  const title = nodes.length === 1 ? `Move ${first.name}` : `Move ${count(nodes.length, 'item')}`
  const sameParent = nodes.every((n) => n.parent_id === first.parent_id)
  return (
    <Dialog title={title} open={props.open} onClose={props.onClose}>
      <ErrorNote error={error} context={{ name: nodes.length === 1 ? first.name : undefined }} />
      <FolderPicker
        tenantId={first.tenant_id}
        startId={first.parent_id ?? undefined}
        disabledIds={nodes.filter((n) => n.kind === 'folder').map((n) => n.id)}
        currentId={sameParent ? (first.parent_id ?? undefined) : undefined}
        pickLabel="Move here"
        onPick={async (folder) => {
          setError(undefined)
          try {
            await props.onMove(folder)
            props.onClose()
          } catch (e) {
            setError(e)
          }
        }}
      />
    </Dialog>
  )
}
