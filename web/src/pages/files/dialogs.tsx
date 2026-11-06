import { useState, type FormEvent } from 'react'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { Dialog, ErrorNote, Loaded } from '../../components/ui'
import { isNoAccess, listSharedWithMe, type Node } from './api'

/** A dialog with one name field, for "New folder" and "Rename". */
export function NameDialog(props: {
  title: string
  label: string
  initial: string
  submitLabel: string
  open: boolean
  onClose: () => void
  onSubmit: (name: string) => Promise<unknown>
  allowEmpty?: boolean
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
        <label>
          {props.label}
          <input
            autoFocus
            required={!props.allowEmpty}
            value={name}
            onChange={(e) => setName(e.target.value)}
            onFocus={(e) => {
              // Select the name without the extension, as file managers do.
              const dot = e.currentTarget.value.lastIndexOf('.')
              e.currentTarget.setSelectionRange(0, dot > 0 ? dot : e.currentTarget.value.length)
            }}
          />
        </label>
        <ErrorNote error={error} />
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
 * Browses the folders of a tenant and returns the picked folder. It starts at `startId`, or at the
 * root. When the user cannot see a folder (a guest, or a folder shared inside a tree), it shows the
 * folders shared with the user in this tenant instead, as the top level.
 */
export function FolderPicker(props: {
  tenantId: string
  startId?: string
  disabledId?: string
  onPick: (folder: Node) => void
  pickLabel: string
}) {
  const { tenantId } = props
  const [folderId, setFolderId] = useState<string | undefined>(props.startId)
  const header = { 'Ruum-Tenant': tenantId }
  const state = useApi(async (): Promise<{ folder: Node | null; folders: Node[] }> => {
    let folder: Node
    try {
      folder = folderId
        ? await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: folderId }, header } }))
        : await unwrap(client.GET('/v1/nodes/root', { params: { header } }))
    } catch (e) {
      if (!isNoAccess(e)) throw e
      // The shared folders of this tenant are the top level the user can see.
      const shared: Node[] = []
      let cursor: string | undefined
      for (let i = 0; i < 20; i++) {
        const page = await listSharedWithMe(cursor)
        shared.push(...page.items.map((s) => s.node).filter((n) => n.tenant_id === tenantId && n.kind === 'folder'))
        if (!page.next_cursor) break
        cursor = page.next_cursor
      }
      return { folder: null, folders: shared }
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
    return { folder, folders }
  }, [folderId, tenantId])

  return (
    <Loaded state={state}>
      {({ folder, folders }) => (
        <div className="stack">
          <p>{folder ? <>In <strong>{folder.parent_id ? folder.name : 'Files'}</strong></> : 'Folders shared with you'}</p>
          <ul className="picker" aria-label="Folders">
            {folder?.parent_id && (
              <li>
                <button type="button" className="link" onClick={() => setFolderId(folder.parent_id!)}>
                  .. (up one level)
                </button>
              </li>
            )}
            {folders.map((f) => (
              <li key={f.id}>
                <button
                  type="button"
                  className="link"
                  disabled={f.id === props.disabledId}
                  onClick={() => setFolderId(f.id)}
                >
                  {f.name}/
                </button>
              </li>
            ))}
            {folders.length === 0 && <li className="muted">No folders here.</li>}
          </ul>
          {folder && (
            <div className="actions">
              <button type="button" className="primary" disabled={folder.id === props.disabledId} onClick={() => props.onPick(folder)}>
                {props.pickLabel}
              </button>
            </div>
          )}
        </div>
      )}
    </Loaded>
  )
}

export function MoveDialog(props: { node: Node; open: boolean; onClose: () => void; onMoved: () => void }) {
  const [error, setError] = useState<unknown>()
  return (
    <Dialog title={`Move ${props.node.name}`} open={props.open} onClose={props.onClose}>
      <ErrorNote error={error} />
      <FolderPicker
        tenantId={props.node.tenant_id}
        startId={props.node.parent_id ?? undefined}
        disabledId={props.node.kind === 'folder' ? props.node.id : undefined}
        pickLabel="Move here"
        onPick={async (folder) => {
          setError(undefined)
          try {
            await unwrap(
              client.POST('/v1/nodes/{node_id}/move', {
                params: { path: { node_id: props.node.id } },
                body: { parent_id: folder.id, on_conflict: 'fail' },
              }),
            )
            props.onMoved()
            props.onClose()
          } catch (e) {
            setError(e)
          }
        }}
      />
    </Dialog>
  )
}
