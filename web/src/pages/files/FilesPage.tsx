import { useEffect, useRef, useState, type ChangeEvent, type DragEvent } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router'
import { client, RuumApiError, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { Actor } from '../../app/names'
import { tenantPath, useTenant } from '../../app/tenant'
import { useSession } from '../../app/useSession'
import { count, formatBytes, formatDate, formatWhen } from '../../components/format'
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
import { listChildren, listSharedWithMe, loadFolder, moveNodes, trashNodes, type Node } from './api'
import { MoveDialog, NameDialog } from './dialogs'
import { NotInSpaceYet } from './ItemPage'
import { NodeDetails } from './NodeDetails'
import { SharedTable } from './SharedPage'
import { nameHeadVersion, replacesOwnSave } from './coalesce'
import { ConflictDialog, type ConflictChoice } from './uploads'
import { useUploads, useUploadsDone, type UploadPlan } from './useUploads'
import { VersionsPanel } from './VersionsPanel'

/** Files whose names are taken, and the folder they go to, while the conflict dialog is open. */
type Pending = { files: File[]; folder: Node; existing: Map<string, Node>; recent: Node[] }

/** The file browser: one folder, its breadcrumbs, its children and the details of one child. */
export function FilesPage() {
  const { folderId } = useParams()
  const { tenant, listed } = useTenant()
  const { me } = useSession()
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

  /** Starts uploads into `folder`; asks once when names are already there (copy.md 3.2). */
  async function startUploads(files: File[], folder: Node, siblings: Node[]) {
    setError(undefined)
    const existing = new Map(siblings.filter((n) => n.kind === 'file').map((n) => [n.name, n]))
    const taken = files.map((f) => existing.get(f.name)).filter((n): n is Node => n !== undefined)
    if (taken.length) {
      // A new version of the person's own unnamed save of the last 10 minutes replaces it
      // (coalescing, design 5.3): the dialog says so before, as "Upload new version" does.
      const checks = await Promise.all(taken.map((n) => replacesOwnSave(n, me.user.id)))
      setPending({ files, folder, existing, recent: taken.filter((_, i) => checks[i]) })
      return
    }
    uploads.add(plansFor(files, folder, existing, 'keep'), folder.tenant_id, folderLabel(folder))
  }

  function plansFor(
    files: File[],
    folder: Node,
    existing: Map<string, Node>,
    choice: ConflictChoice,
    named = new Set<string>(),
  ): UploadPlan[] {
    const plans: UploadPlan[] = []
    for (const file of files) {
      const old = existing.get(file.name)
      if (old && choice === 'skip') continue
      if (old && choice === 'version') {
        const mine = old.updated_by.actor.kind === 'user' && old.updated_by.actor.id === me.user.id
        plans.push({
          file,
          target: { target: 'new_version', node_id: old.id, base_version: old.head_version },
          previousAt: mine && !named.has(old.id) ? old.updated_at : undefined,
        })
      } else {
        plans.push({ file, target: { target: 'new_file', parent_id: folder.id, name: file.name, on_conflict: 'rename' } })
      }
    }
    return plans
  }

  function folderLabel(folder: Node): string {
    return folder.parent_id ? folder.name : 'Files'
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
      const what = done.length === 1 ? done[0].name : count(done.length, 'item')
      toast(`Moved ${what} to the trash.`, {
        undo: async () => {
          await undo()
          reload()
          return done.length === 1 ? `Restored ${done[0].name}.` : `Restored ${count(done.length, 'item')}.`
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
    const what = done.length === 1 ? done[0].name : count(done.length, 'item')
    toast(`Moved ${what} to ${folderLabel(target)}.`, {
      undo: async () => {
        await undo()
        reload()
        return done.length === 1 ? `Moved ${done[0].name} back.` : `Moved ${count(done.length, 'item')} back.`
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
      asideLabel="Details"
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
        <Loading label="Loading items" />
      ) : state.error instanceof RuumApiError && state.error.status === 404 && listed === false ? (
        <NotInSpaceYet />
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
              <DropZone active={dropTarget === folder.id} hint={`Drop to upload to ${folderLabel(folder)}`} {...dropProps}>
                <Breadcrumbs hidden={ancestors.length < 2}>
                  {ancestors.map((a, i) => {
                    const label = a.parent_id ? a.name : 'Files'
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
                  <h1>{isRoot ? 'Files' : folder.name}</h1>
                  {canEdit && (
                    <Actions>
                      <Button onClick={() => setNewFolder(true)}>New folder</Button>
                      <Button variant="primary" icon="upload" onClick={() => fileInput.current?.click()}>
                        Upload files
                      </Button>
                      <FileInput ref={fileInput} multiple label="Upload files" onChange={onUpload} />
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
                  <SelectionBar label="Selection">
                    <span>{count(checkedNodes.length, 'item')} selected</span>
                    {canEdit && (
                      <>
                        <Button size="sm" onClick={() => setMoving(checkedNodes)}>
                          Move
                        </Button>
                        <Button size="sm" onClick={() => trash(checkedNodes)}>
                          Move to trash
                        </Button>
                      </>
                    )}
                    <Button variant="link" onClick={() => setChecked(new Set())}>
                      Clear selection
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
                    title="New folder"
                    label="Folder name"
                    initial=""
                    placeholder="Site photos"
                    submitLabel="Create"
                    onClose={() => setNewFolder(false)}
                    onSubmit={async (name) => {
                      await unwrap(client.POST('/v1/nodes', { body: { parent_id: folder.id, name, kind: 'folder' } }))
                      toast(`Folder ${name} created.`)
                      reload()
                    }}
                  />
                )}
                {pending && (
                  <ConflictDialog
                    names={pending.files.filter((f) => pending.existing.has(f.name)).map((f) => f.name)}
                    recent={pending.recent}
                    onClose={() => setPending(null)}
                    onChoose={async (choice, label) => {
                      // A name keeps the person's save of the last minutes as its own version.
                      const named = new Set<string>()
                      if (choice === 'version' && label) {
                        for (const n of pending.recent) {
                          await nameHeadVersion(n, label)
                          named.add(n.id)
                        }
                      }
                      const plans = plansFor(pending.files, pending.folder, pending.existing, choice, named)
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
      <h2 id="welcome-title">Welcome to Ruum, {first}</h2>
      <ProseList>
        <li>Upload files from any device; drop them on a folder to put them there.</li>
        <li>Every change is kept as a version, so you can go back.</li>
        <li>Connect an AI agent that works for you, only in the folders you choose.</li>
      </ProseList>
      <Actions align="start">
        <Button variant="primary" onClick={props.onUpload}>
          Upload files
        </Button>
        <Button onClick={props.onConnect}>Connect an agent</Button>
        <Button onClick={close}>Close</Button>
      </Actions>
    </Card>
  )
}

function EmptyFolder(props: { isRoot: boolean; canEdit: boolean; onUpload: () => void; onNewFolder: () => void }) {
  if (!props.canEdit) return <EmptyState icon="info" title="This folder is empty" />
  const actions = (
    <>
      <Button variant="primary" onClick={props.onUpload}>
        Upload files
      </Button>
      <Button onClick={props.onNewFolder}>New folder</Button>
    </>
  )
  return props.isRoot ? (
    <EmptyState title="Your files go here" text="Upload files or make folders. Every change is kept as a version.">
      {actions}
    </EmptyState>
  ) : (
    <EmptyState title="This folder is empty" text="Drop files here, or upload them.">
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
      <h1>Shared with you in {spaceName}</h1>
      <Loaded state={shared}>
        {({ items }) => {
          const here = items.filter((s) => s.node.tenant_id === tenantId)
          return here.length === 0 ? (
            <EmptyState title="Nothing yet" text={`You see only what people in ${spaceName} shared with you.`} />
          ) : (
            <>
              <Text tone="muted">You see only what people in {spaceName} shared with you.</Text>
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

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

/** Folders first, then by the key; names in natural order ("photo 2" before "photo 10"). */
function sortNodes(nodes: Node[], key: SortKey, dir: 1 | -1): Node[] {
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
    <Table caption="Contents of this folder">
      <THead>
        <Tr>
          <Th scope="col" check>
            <BareCheckbox
              aria-label="Select all"
              checked={all}
              ref={(el) => {
                if (el) el.indeterminate = some && !all
              }}
              onChange={(e) => props.onToggleAll(e.target.checked)}
            />
          </Th>
          {header('name', 'Name')}
          {header('size', 'Size', true)}
          {header('changed', 'Changed', true)}
          <Th scope="col">
            <VisuallyHidden>Actions</VisuallyHidden>
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
                  aria-label={`Select ${n.name}`}
                  checked={props.checked.has(n.id)}
                  onChange={(e) => props.onToggle(n.id, e.target.checked)}
                />
              </Td>
              <Th scope="row">
                <NameCell icon={<KindIcon kind={n.kind} type={n.type} name={n.name} />}>
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
                <Button size="sm" aria-label={`Details of ${n.name}`} onClick={() => props.onSelect(n)}>
                  Details
                </Button>
              </Td>
            </Tr>
          )
        })}
      </TBody>
    </Table>
  )
}
