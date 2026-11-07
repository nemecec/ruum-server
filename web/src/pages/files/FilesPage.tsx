import { useEffect, useRef, useState, type ChangeEvent, type DragEvent } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { client, RuumApiError, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { Actor } from '../../app/names'
import { tenantPath, useTenant } from '../../app/tenant'
import { useSession } from '../../app/useSession'
import { formatBytes, formatDate, formatWhen } from '../../components/format'
import {
  Actions,
  BareCheckbox,
  Breadcrumbs,
  Button,
  Card,
  Crumb,
  CurrentCrumb,
  DropZone,
  EmptyState,
  ErrorNote,
  FileInput,
  KindIcon,
  Loaded,
  Loading,
  LoadMore,
  NameCell,
  ProseList,
  SelectionBar,
  SortHeader,
  SplitView,
  Table,
  TBody,
  Td,
  Text,
  Th,
  THead,
  Toolbar,
  Tr,
  useToast,
  VisuallyHidden,
} from '../../ui'
import { findFilesByName, listChildren, listSharedWithMe, loadFolder, moveNodes, nameKey, newVersionTarget, refreshNode, trashNodes, type Node } from './api'
import { MoveDialog, NameDialog } from './dialogs'
import { focusPageHeading } from '../../components/dom'
import { EmailShareCheck } from './EmailShareCheck'
import { NodeDetails } from './NodeDetails'
import { SharedTable } from './SharedPage'
import { ConflictDialog, type ConflictChoice } from './uploads'
import { useUploads, useUploadsDone, type UploadPlan } from './useUploads'
import { VersionsPanel } from './VersionsPanel'
import { currentIntl, t } from '../../i18n'

/**
 * Files whose names are taken, and the folder they go to, while the conflict dialog is open.
 * `existing` maps `nameKey` to the file of that name. `again` is set when the server answered
 * `name_taken` for files of an earlier plan (see `askAgain`).
 */
type Pending = { files: File[]; folder: Node; existing: Map<string, Node>; again: boolean }

/** The file browser: one folder, its breadcrumbs, its children and the details of one child. */
export function FilesPage() {
  const { folderId } = useParams()
  const { tenant, listed } = useTenant()
  const { toast } = useToast()
  const navigate = useNavigate()
  const [params, setParams] = useSearchParams()
  const selectedId = params.get('node')
  const panel = params.get('panel')
  const [error, setError] = useState<unknown>()
  const [newFolder, setNewFolder] = useState(false)
  const [checked, setChecked] = useState<Set<string>>(new Set())
  const [moving, setMoving] = useState<Node[] | null>(null)
  const [pending, setPending] = useState<Pending | null>(null)
  const [dropTarget, setDropTarget] = useState<string | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const uploads = useUploads()

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
  useUploadsDone(reload)

  // While another folder loads, the page must not keep the toolbar of the one before: an upload
  // started then would land in the wrong folder.
  const stale = state.data !== undefined && (folderId ? state.data.folder?.id !== folderId : Boolean(state.data.folder?.parent_id))

  const items = children.data?.items ?? []
  const selected = items.find((n) => n.id === selectedId) ?? null
  const checkedNodes = items.filter((n) => checked.has(n.id))

  function select(node: Node | null, nextPanel?: string) {
    const next = new URLSearchParams(params)
    if (node) next.set('node', node.id)
    else next.delete('node')
    if (nextPanel) next.set('panel', nextPanel)
    else next.delete('panel')
    setParams(next, { replace: true })
  }

  // What the page shows now, for uploads that the server answers after a while (`askAgain`).
  const here = useRef<{ folderId?: string; asking: boolean }>({ asking: false })
  useEffect(() => {
    here.current = { folderId: state.data?.folder?.id, asking: pending !== null }
  })
  useEffect(() => () => {
    here.current = { asking: false }
  }, [])

  /**
   * Starts uploads into `folder`; asks once when names are already there (copy.md 3.2).
   * `siblings` can be a moment old (the list reloads after an upload), so the plan never trusts
   * it alone: a file that looks new goes with `on_conflict: fail`, and a `name_taken` answer
   * brings it back to the dialog (`askAgain`). `again` is set for such files.
   */
  async function startUploads(files: File[], folder: Node, siblings: Node[], again = false) {
    setError(undefined)
    const existing = new Map(siblings.filter((n) => n.kind === 'file').map((n) => [nameKey(n.name), n]))
    const taken = [...new Set(files.map((f) => existing.get(nameKey(f.name))).filter((n): n is Node => n !== undefined))]
    if (taken.length) {
      // The new version starts from the head the server has now, not from the list on the screen.
      const fresh = await Promise.all(taken.map(refreshNode))
      taken.forEach((old, i) => existing.set(nameKey(old.name), fresh[i]))
      setPending({ files, folder, existing, again })
      return
    }
    uploads.add(plansFor(files, folder, existing, 'keep', again), folder.tenant_id, folderLabel(folder))
  }

  /**
   * The server answered `name_taken` for `files`: their names were taken after the page read its
   * list (for example by the upload just before, while the list reloaded). Read the names again
   * and ask, so that no upload replaces or renames a file without the person's answer. Returns
   * `false` when the page cannot ask now: it shows another folder or another question.
   */
  function askAgain(files: File[], folder: Node): boolean {
    if (here.current.folderId !== folder.id || here.current.asking) return false
    here.current = { ...here.current, asking: true }
    void (async () => {
      try {
        await startUploads(files, folder, await findFilesByName(folder, files.map((f) => f.name)), true)
      } catch (err) {
        setError(err)
      }
    })()
    return true
  }

  function plansFor(
    files: File[],
    folder: Node,
    existing: Map<string, Node>,
    choice: ConflictChoice,
    again = false,
  ): UploadPlan[] {
    const plans: UploadPlan[] = []
    // One callback for the batch, so the queue gives its files back together, in one question.
    const onNameTaken = (taken: File[]) => askAgain(taken, folder)
    for (const file of files) {
      const old = existing.get(nameKey(file.name))
      if (old && choice === 'skip') continue
      if (old && choice === 'version') {
        // An explicit new version never joins the version before (C-8).
        plans.push({ file, target: newVersionTarget(old) })
      } else if (old || again) {
        // "Keep both", or a name that is still taken by something that is not a file (a folder).
        plans.push({ file, target: { target: 'new_file', parent_id: folder.id, name: file.name, on_conflict: 'rename' } })
      } else {
        plans.push({
          file,
          target: { target: 'new_file', parent_id: folder.id, name: file.name, on_conflict: 'fail' },
          onNameTaken,
        })
      }
    }
    return plans
  }

  function folderLabel(folder: Node): string {
    return folder.parent_id ? folder.name : t('shell.nav.files')
  }

  function onUpload(e: ChangeEvent<HTMLInputElement>) {
    const files = Array.from(e.target.files ?? [])
    e.target.value = ''
    const folder = state.data?.folder
    if (!files.length || !folder || stale) return
    // A click on a folder changes the address at once, but this page renders the new folder a
    // moment later. Files picked in that moment wait for the folder of the address.
    if (addressFolderChanged(folder)) {
      waiting.current = files
      return
    }
    void startUploads(files, folder, items)
  }

  // Files that waited for the folder of the address start when its children are on the screen.
  const waiting = useRef<File[] | null>(null)
  useEffect(() => {
    const folder = state.data?.folder
    if (!waiting.current || stale || !folder || !children.data || children.loading) return
    if (addressFolderChanged(folder)) return
    const files = waiting.current
    waiting.current = null
    void startUploads(files, folder, children.data.items)
  })

  async function onDropInto(e: DragEvent, target: Node) {
    e.preventDefault()
    e.stopPropagation()
    setDropTarget(null)
    const files = Array.from(e.dataTransfer.files ?? [])
    if (!files.length) return
    if (target.id === state.data?.folder?.id) {
      await startUploads(files, target, items)
    } else {
      // A folder row: its children decide the conflicts.
      try {
        const page = await listChildren(target.id, undefined, 'file')
        await startUploads(files, target, page.items)
      } catch (err) {
        setError(err)
      }
    }
  }

  async function trash(nodes: Node[]) {
    setError(undefined)
    try {
      const { done, undo } = await trashNodes(nodes)
      setChecked(new Set())
      if (nodes.some((n) => n.id === selectedId)) select(null)
      reload()
      focusPageHeading()
      toast(done.length === 1 ? t('files.trashed.one', { name: done[0].name }) : t('files.trashed', { n: done.length }), {
        undo: async () => {
          await undo()
          reload()
          return done.length === 1 ? t('files.restored.one', { name: done[0].name }) : t('files.restored', { n: done.length })
        },
      })
    } catch (err) {
      setError(err)
      reload()
    }
  }

  async function move(nodes: Node[], target: Node) {
    const { done, undo } = await moveNodes(nodes, target)
    setChecked(new Set())
    if (nodes.some((n) => n.id === selectedId)) select(null)
    reload()
    focusPageHeading()
    const folder = folderLabel(target)
    toast(done.length === 1 ? t('files.moved.one', { name: done[0].name, folder }) : t('files.moved', { n: done.length, folder }), {
      undo: async () => {
        await undo()
        reload()
        return done.length === 1 ? t('files.movedBack.one', { name: done[0].name }) : t('files.movedBack', { n: done.length })
      },
    })
  }

  function toggle(id: string, on: boolean) {
    setChecked((s) => {
      const next = new Set(s)
      if (on) next.add(id)
      else next.delete(id)
      return next
    })
  }

  return (
    <SplitView
      asideLabel={t('files.details')}
      onCloseAside={() => select(null)}
      aside={
        selected && !stale ? (
          panel === 'versions' && selected.kind === 'file' ? (
            <VersionsPanel node={selected} onClose={() => select(selected)} onRestored={reload} />
          ) : (
            <NodeDetails
              key={selected.id}
              node={selected}
              onChanged={reload}
              onTrash={() => trash([selected])}
              onMove={() => setMoving([selected])}
              onShowVersions={() => select(selected, 'versions')}
              onClose={() => select(null)}
            />
          )
        ) : undefined
      }
    >
      {stale ? (
        <Loading label={t('files.loadingItems')} />
      ) : state.error instanceof RuumApiError && state.error.status === 404 && (folderId || listed === false) ? (
        // A folder of a share email: a share to the person's email may need a bind check first.
        <EmailShareCheck nodeId={folderId ?? tenant.id} tenantId={tenant.id} onRetry={state.reload} loading={state.loading} />
      ) : (
        <Loaded state={state}>
          {({ folder, ancestors }) => {
            if (!folder) return <GuestHome tenantId={tenant.id} spaceName={tenant.name} />
            const canEdit = folder.my_role === 'edit' || folder.my_role === 'manage'
            const isRoot = !folder.parent_id
            const dropProps = canEdit
              ? {
                  onDragOver: (e: DragEvent) => {
                    if (!e.dataTransfer.types.includes('Files')) return
                    e.preventDefault()
                    setDropTarget(folder.id)
                  },
                  onDragLeave: (e: DragEvent) => {
                    if (!e.currentTarget.contains(e.relatedTarget as globalThis.Node | null)) setDropTarget(null)
                  },
                  onDrop: (e: DragEvent) => void onDropInto(e, folder),
                }
              : {}
            return (
              <DropZone active={dropTarget === folder.id} hint={t('files.dropHint', { folder: folderLabel(folder) })} {...dropProps}>
                <Breadcrumbs hidden={ancestors.length < 2}>
                  {ancestors.map((a, i) => {
                    const label = a.parent_id ? a.name : t('shell.nav.files')
                    return i === ancestors.length - 1 ? (
                      <CurrentCrumb key={a.id}>{label}</CurrentCrumb>
                    ) : (
                      <Crumb key={a.id}>
                        <Link to={a.parent_id ? tenantPath(tenant.id, 'files', a.id) : tenantPath(tenant.id, 'files')}>{label}</Link>
                      </Crumb>
                    )
                  })}
                </Breadcrumbs>
                <Toolbar>
                  <h1>{isRoot ? t('shell.nav.files') : folder.name}</h1>
                  {canEdit && (
                    <Actions>
                      <Button onClick={() => setNewFolder(true)}>{t('files.newFolder')}</Button>
                      <Button variant="primary" icon="upload" onClick={() => fileInput.current?.click()}>
                        {t('files.upload')}
                      </Button>
                      <FileInput ref={fileInput} multiple label={t('files.upload')} onChange={onUpload} />
                    </Actions>
                  )}
                </Toolbar>
                {isRoot && tenant.kind === 'personal' && (
                  <WelcomeCard
                    empty={children.data?.items.length === 0}
                    onUpload={() => fileInput.current?.click()}
                    onConnect={() => navigate(tenantPath(tenant.id, 'agents', 'connect'))}
                  />
                )}
                <ErrorNote error={error} />
                {checkedNodes.length > 0 && (
                  <SelectionBar label={t('files.selection')}>
                    <span>{t('files.selected', { n: checkedNodes.length })}</span>
                    {canEdit && (
                      <>
                        <Button size="sm" onClick={() => setMoving(checkedNodes)}>
                          {t('files.move')}
                        </Button>
                        <Button size="sm" onClick={() => trash(checkedNodes)}>
                          {t('files.toTrash')}
                        </Button>
                      </>
                    )}
                    <Button variant="link" onClick={() => setChecked(new Set())}>
                      {t('files.clearSelection')}
                    </Button>
                  </SelectionBar>
                )}
                <Loaded state={children}>
                  {({ items }) =>
                    items.length === 0 ? (
                      <EmptyFolder
                        isRoot={isRoot}
                        canEdit={canEdit}
                        onUpload={() => fileInput.current?.click()}
                        onNewFolder={() => setNewFolder(true)}
                      />
                    ) : (
                      <FileTable
                        nodes={items}
                        tenantId={tenant.id}
                        selectedId={selectedId}
                        checked={checked}
                        canEdit={canEdit}
                        dropTarget={dropTarget}
                        onToggle={toggle}
                        onToggleAll={(on) => setChecked(on ? new Set(items.map((n) => n.id)) : new Set())}
                        onSelect={(n) => select(n)}
                        onDragOverFolder={(id) => setDropTarget(id)}
                        onDropFolder={(e, n) => void onDropInto(e, n)}
                      />
                    )
                  }
                </Loaded>
                <LoadMore state={children} />
                {newFolder && (
                  <NameDialog
                    open
                    title={t('files.newFolder')}
                    label={t('files.newFolder.name')}
                    initial=""
                    placeholder={t('files.newFolder.placeholder')}
                    submitLabel={t('common.create')}
                    onClose={() => setNewFolder(false)}
                    onSubmit={async (name) => {
                      await unwrap(client.POST('/v1/nodes', { body: { parent_id: folder.id, name, kind: 'folder' } }))
                      toast(t('files.newFolder.done', { name }))
                      reload()
                    }}
                  />
                )}
                {pending && (
                  <ConflictDialog
                    names={pending.files.filter((f) => pending.existing.has(nameKey(f.name))).map((f) => f.name)}
                    onClose={() => setPending(null)}
                    onChoose={(choice) => {
                      const plans = plansFor(pending.files, pending.folder, pending.existing, choice, pending.again)
                      setPending(null)
                      if (plans.length) uploads.add(plans, pending.folder.tenant_id, folderLabel(pending.folder))
                    }}
                  />
                )}
                {moving && <MoveDialog nodes={moving} open onClose={() => setMoving(null)} onMove={(target) => move(moving, target)} />}
              </DropZone>
            )
          }}
        </Loaded>
      )}
    </SplitView>
  )
}

/**
 * True when the browser's address names another folder than `folder` (the router changed it, and
 * this page has not rendered it yet). Only for the browser router: in tests the address stays.
 */
function addressFolderChanged(folder: Node): boolean {
  const path = window.location.pathname
  if (!/^\/t\/[^/]+\/files/.test(path)) return false
  const id = /\/files\/([0-9a-f-]{36})/.exec(path)?.[1]
  return id ? id !== folder.id : Boolean(folder.parent_id)
}

const WELCOME_KEY = 'ruum.welcomeClosed'

function welcomeClosed(userId: string): boolean {
  try {
    return localStorage.getItem(`${WELCOME_KEY}.${userId}`) === '1'
  } catch {
    return false
  }
}

/**
 * The welcome card (copy.md 3.1, journeys J1): on the empty personal space after the first
 * sign-in, or with `?welcome=1`. It does not come back after Close.
 */
function WelcomeCard(props: { empty: boolean | undefined; onUpload: () => void; onConnect: () => void }) {
  const { me } = useSession()
  const [params] = useSearchParams()
  const [closed, setClosed] = useState(() => welcomeClosed(me.user.id))
  const asked = params.get('welcome') === '1'
  if (closed || !(asked || props.empty)) return null
  const first = me.user.display_name.split(/\s+/)[0]
  function close() {
    try {
      localStorage.setItem(`${WELCOME_KEY}.${me.user.id}`, '1')
    } catch {
      // Storage can be off; the card then comes back on the next visit.
    }
    setClosed(true)
  }
  return (
    <Card aria-labelledby="welcome-title">
      <h2 id="welcome-title">{t('files.welcome.title', { name: first })}</h2>
      <ProseList>
        <li>{t('files.welcome.upload')}</li>
        <li>{t('files.welcome.versions')}</li>
        <li>{t('files.welcome.agent')}</li>
      </ProseList>
      <Actions align="start">
        <Button variant="primary" onClick={props.onUpload}>
          {t('files.upload')}
        </Button>
        <Button onClick={props.onConnect}>{t('files.welcome.connect')}</Button>
        <Button onClick={close}>{t('ui.close')}</Button>
      </Actions>
    </Card>
  )
}

function EmptyFolder(props: { isRoot: boolean; canEdit: boolean; onUpload: () => void; onNewFolder: () => void }) {
  if (!props.canEdit) return <EmptyState icon="info" title={t('files.empty.title')} />
  const actions = (
    <>
      <Button variant="primary" onClick={props.onUpload}>
        {t('files.upload')}
      </Button>
      <Button onClick={props.onNewFolder}>{t('files.newFolder')}</Button>
    </>
  )
  return props.isRoot ? (
    <EmptyState title={t('files.emptyRoot.title')} text={t('files.emptyRoot.text')}>
      {actions}
    </EmptyState>
  ) : (
    <EmptyState title={t('files.empty.title')} text={t('files.empty.text')}>
      {actions}
    </EmptyState>
  )
}

/**
 * The files page of a space whose top the person cannot see (a guest): the items shared with them
 * in this space.
 */
function GuestHome({ tenantId, spaceName }: { tenantId: string; spaceName: string }) {
  const shared = usePagedApi(listSharedWithMe, [])
  return (
    <>
      <h1>{t('files.guest.title', { space: spaceName })}</h1>
      <Loaded state={shared}>
        {({ items }) => {
          const here = items.filter((s) => s.node.tenant_id === tenantId)
          return here.length === 0 ? (
            <EmptyState title={t('files.guest.none')} text={t('files.guest.text', { space: spaceName })} />
          ) : (
            <>
              <Text tone="muted">{t('files.guest.text', { space: spaceName })}</Text>
              <SharedTable items={here} />
            </>
          )
        }}
      </Loaded>
      <LoadMore state={shared} />
    </>
  )
}

type SortKey = 'name' | 'size' | 'changed'

/** Folders first, then by the key; names in natural order ("photo 2" before "photo 10"), in the UI's locale. */
function sortNodes(nodes: Node[], key: SortKey, dir: 1 | -1): Node[] {
  const collator = new Intl.Collator(currentIntl().locale, { numeric: true, sensitivity: 'base' })
  return [...nodes].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === 'folder' ? -1 : 1
    const by =
      key === 'size'
        ? a.size_bytes - b.size_bytes
        : key === 'changed'
          ? a.updated_at.localeCompare(b.updated_at)
          : collator.compare(a.name, b.name)
    return (by || collator.compare(a.name, b.name)) * dir
  })
}

function FileTable(props: {
  nodes: Node[]
  tenantId: string
  selectedId: string | null
  checked: Set<string>
  canEdit: boolean
  dropTarget: string | null
  onToggle: (id: string, on: boolean) => void
  onToggleAll: (on: boolean) => void
  onSelect: (n: Node) => void
  onDragOverFolder: (id: string | null) => void
  onDropFolder: (e: DragEvent, n: Node) => void
}) {
  const [sort, setSort] = useState<{ key: SortKey; dir: 1 | -1 }>({ key: 'name', dir: 1 })
  const nodes = sortNodes(props.nodes, sort.key, sort.dir)
  const all = props.nodes.length > 0 && props.nodes.every((n) => props.checked.has(n.id))
  const some = props.nodes.some((n) => props.checked.has(n.id))
  const header = (key: SortKey, label: string, hideNarrow?: boolean) => (
    <SortHeader
      label={label}
      hideNarrow={hideNarrow}
      sort={sort.key === key ? (sort.dir === 1 ? 'ascending' : 'descending') : 'none'}
      onSort={() => setSort((s) => (s.key === key ? { key, dir: s.dir === 1 ? -1 : 1 } : { key, dir: key === 'name' ? 1 : -1 }))}
    />
  )
  return (
    <Table caption={t('files.table.caption')}>
      <THead>
        <Tr>
          <Th scope="col" check>
            <BareCheckbox
              aria-label={t('files.selectAll')}
              checked={all}
              ref={(el) => {
                if (el) el.indeterminate = some && !all
              }}
              onChange={(e) => props.onToggleAll(e.target.checked)}
            />
          </Th>
          {header('name', t('common.name'))}
          {header('size', t('files.size'), true)}
          {header('changed', t('files.changed'), true)}
          <Th scope="col">
            <VisuallyHidden>{t('common.actions')}</VisuallyHidden>
          </Th>
        </Tr>
      </THead>
      <TBody>
        {nodes.map((n) => {
          const folderDrop =
            n.kind === 'folder' && props.canEdit
              ? {
                  onDragOver: (e: DragEvent) => {
                    if (!e.dataTransfer.types.includes('Files')) return
                    e.preventDefault()
                    e.stopPropagation()
                    props.onDragOverFolder(n.id)
                  },
                  onDrop: (e: DragEvent) => props.onDropFolder(e, n),
                }
              : {}
          return (
            <Tr
              key={n.id}
              selected={n.id === props.selectedId || props.checked.has(n.id)}
              dropTarget={props.dropTarget === n.id}
              {...folderDrop}
            >
              <Td check>
                <BareCheckbox
                  aria-label={t('files.select.named', { name: n.name })}
                  checked={props.checked.has(n.id)}
                  onChange={(e) => props.onToggle(n.id, e.target.checked)}
                />
              </Td>
              <Th scope="row">
                <NameCell
                  icon={<KindIcon kind={n.kind} type={n.type} name={n.name} />}
                  meta={
                    // On the phone the Changed column is hidden: when and who (an agent with its
                    // mark) go under the name, so the agent is visible in one look (journey J5).
                    <>
                      <span title={formatDate(n.updated_at)}>{formatWhen(n.updated_at)}</span> · <Actor pair={n.updated_by} />
                    </>
                  }
                >
                  {n.kind === 'folder' ? (
                    <Link to={tenantPath(props.tenantId, 'files', n.id)}>{n.name}</Link>
                  ) : (
                    <Button variant="link" onClick={() => props.onSelect(n)}>
                      {n.name}
                    </Button>
                  )}
                </NameCell>
              </Th>
              <Td hideNarrow numeric>
                {n.kind === 'folder' ? '-' : formatBytes(n.size_bytes)}
              </Td>
              <Td hideNarrow>
                <span title={formatDate(n.updated_at)}>{formatWhen(n.updated_at)}</span>
                <Text as="span" tone="muted" size="sm" block>
                  <Actor pair={n.updated_by} />
                </Text>
              </Td>
              <Td actions>
                <Button size="sm" aria-label={t('files.details.named', { name: n.name })} onClick={() => props.onSelect(n)}>
                  {t('files.details')}
                </Button>
              </Td>
            </Tr>
          )
        })}
      </TBody>
    </Table>
  )
}
