import { useState, type FormEvent } from 'react'
import { client, RuumApiError, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import {
  Actions,
  Breadcrumbs,
  Button,
  Crumb,
  CurrentCrumb,
  Dialog,
  ErrorNote,
  Field,
  FolderIcon,
  Loaded,
  Picker,
  PickerEmpty,
  PickerItem,
  Stack,
  Text,
  TextInput,
} from '../../ui'
import { isNoAccess, listSharedWithMe, loadAncestors, type Node } from './api'
import { t } from '../../i18n'

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
      <Stack as="form" gap={4} onSubmit={submit}>
        {props.help && <Text tone="muted">{props.help}</Text>}
        <Field label={props.label}>
          <TextInput
            required={!props.allowEmpty}
            value={name}
            placeholder={props.placeholder}
            data-select-base={props.selectBase ? '' : undefined}
            onChange={(e) => setName(e.target.value)}
          />
        </Field>
        <ErrorNote error={error} context={{ name: name.trim() }} />
        <Actions>
          <Button onClick={props.onClose}>{t('common.cancel')}</Button>
          <Button type="submit" variant="primary" disabled={busy || (!props.allowEmpty && !name.trim())} busy={busy}>
            {props.submitLabel}
          </Button>
        </Actions>
      </Stack>
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
      // i18n-ignore: an internal signal, never shown
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
        const reason = inside ? t('picker.inside') : here ? t('picker.here') : null
        const canCreate = folder && (folder.my_role === 'edit' || folder.my_role === 'manage')
        return (
          <Stack>
            <Breadcrumbs label={t('picker.path')}>
              {folder && ancestors[0]?.parent_id && (
                <Crumb>
                  <Button variant="link" onClick={() => setFolderId(null)}>
                    {t('picker.shared')}
                  </Button>
                </Crumb>
              )}
              {folder ? (
                ancestors.map((a, i) => {
                  const label = a.parent_id ? a.name : t('shell.nav.files')
                  return i === ancestors.length - 1 ? (
                    <CurrentCrumb key={a.id} current="location">
                      {label}
                    </CurrentCrumb>
                  ) : (
                    <Crumb key={a.id}>
                      <Button variant="link" onClick={() => setFolderId(a.id)}>
                        {label}
                      </Button>
                    </Crumb>
                  )
                })
              ) : (
                <CurrentCrumb current="location">{t('picker.shared')}</CurrentCrumb>
              )}
            </Breadcrumbs>
            <Picker label={t('picker.folders')}>
              {folders.map((f) => (
                <PickerItem key={f.id} icon={<FolderIcon />} disabled={disabled.has(f.id)} onClick={() => setFolderId(f.id)}>
                  {f.name}
                </PickerItem>
              ))}
              {folders.length === 0 && <PickerEmpty>{t('picker.empty')}</PickerEmpty>}
            </Picker>
            {reason && (
              <Text tone="muted" size="sm">
                {reason}
              </Text>
            )}
            {folder && (
              <Actions>
                {canCreate && <Button onClick={() => setCreating(true)}>{t('files.newFolder')}</Button>}
                <Button
                  variant="primary"
                  busy={busy}
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
                </Button>
              </Actions>
            )}
            {creating && folder && (
              <NameDialog
                open
                title={t('files.newFolder')}
                label={t('files.newFolder.name')}
                initial=""
                submitLabel={t('common.create')}
                onClose={() => setCreating(false)}
                onSubmit={async (name) => {
                  const res = await unwrap(
                    client.POST('/v1/nodes', { params: { header }, body: { parent_id: folder.id, name, kind: 'folder' } }),
                  )
                  setFolderId(res.node.id)
                }}
              />
            )}
          </Stack>
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
  const title = nodes.length === 1 ? t('move.title.one', { name: first.name }) : t('move.title', { n: nodes.length })
  const sameParent = nodes.every((n) => n.parent_id === first.parent_id)
  return (
    <Dialog title={title} open={props.open} onClose={props.onClose}>
      <Stack gap={4}>
        <ErrorNote error={error} context={{ name: nodes.length === 1 ? first.name : undefined }} />
        <FolderPicker
          tenantId={first.tenant_id}
          startId={first.parent_id ?? undefined}
          disabledIds={nodes.filter((n) => n.kind === 'folder').map((n) => n.id)}
          currentId={sameParent ? (first.parent_id ?? undefined) : undefined}
          pickLabel={t('move.here')}
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
      </Stack>
    </Dialog>
  )
}
