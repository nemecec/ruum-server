import { useEffect, useRef, useState } from 'react'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { Actor } from '../../app/names'
import { useSession } from '../../app/useSession'
import { fileTypeLabel, formatBytes, formatDate, formatWhen, roleLabels } from '../../components/format'
import {
  Actions,
  Button,
  Dialog,
  ErrorNote,
  Fact,
  Facts,
  Field,
  FileInput,
  IconButton,
  ListEmpty,
  Loaded,
  Select,
  Stack,
  Tag,
  TagList,
  TextInput,
  Toolbar,
  useToast,
} from '../../ui'
import { downloadNode, previewNode, type Node } from './api'
import { NameDialog } from './dialogs'
import { ShareDialog } from './ShareDialog'
import { nameHeadVersion, replacesOwnSave } from './coalesce'
import { useUploads } from './useUploads'
import { t, tr } from '../../i18n'

/** The details of an item and its actions, shown beside the file list. */
export function NodeDetails(props: {
  node: Node
  onChanged: () => void
  onTrash: () => void
  onMove: () => void
  onShowVersions: () => void
  onClose?: () => void
}) {
  const { node } = props
  const { me } = useSession()
  const { toast } = useToast()
  const uploads = useUploads()
  const [dialog, setDialog] = useState<'rename' | 'share' | null>(null)
  const [error, setError] = useState<unknown>()
  const [versionFile, setVersionFile] = useState<{ file: File; previousAt: string } | null>(null)
  const canEdit = node.my_role === 'edit' || node.my_role === 'manage'
  const heading = useRef<HTMLHeadingElement>(null)
  const versionInput = useRef<HTMLInputElement>(null)

  // Move the focus to the panel when it opens, so keyboard and phone users land on it.
  useEffect(() => {
    heading.current?.focus()
    heading.current?.scrollIntoView?.({ block: 'start', behavior: 'smooth' })
  }, [node.id])

  function uploadVersion(file: File, previousAt?: string) {
    uploads.add(
      [{ file, target: { target: 'new_version', node_id: node.id, base_version: node.head_version }, previousAt }],
      node.tenant_id,
    )
  }

  /**
   * "Upload new version": when the person's own last save is younger than the coalescing window,
   * the server would join this upload into it and the old content would be gone. Ask first.
   */
  async function chooseVersion(file: File) {
    setError(undefined)
    if (await replacesOwnSave(node, me.user.id)) {
      setVersionFile({ file, previousAt: node.updated_at })
      return
    }
    uploadVersion(file)
  }

  return (
    <Stack as="section" gap={4} aria-labelledby="details-title">
      <Toolbar>
        <h2 id="details-title" ref={heading} tabIndex={-1}>
          {node.name}
        </h2>
        {props.onClose && <IconButton icon="close" label={t('details.close')} onClick={props.onClose} />}
      </Toolbar>
      <Facts>
        <Fact label={t('details.type')}>{fileTypeLabel(node)}</Fact>
        {node.kind === 'file' && <Fact label={t('files.size')}>{formatBytes(node.size_bytes)}</Fact>}
        <Fact label={t('files.changed')}>
          {tr('details.when.by', {
            when: <span title={formatDate(node.updated_at)}>{formatWhen(node.updated_at)}</span>,
            who: <Actor pair={node.updated_by} />,
          })}
        </Fact>
        <Fact label={t('details.created')}>
          {tr('details.when.by', {
            when: <span title={formatDate(node.created_at)}>{formatWhen(node.created_at)}</span>,
            who: <Actor pair={node.created_by} />,
          })}
        </Fact>
        {node.my_role && <Fact label={t('shared.access')}>{roleLabels[node.my_role]}</Fact>}
      </Facts>
      <ErrorNote error={error} context={{ name: node.name }} />
      <Actions align="start">
        {node.kind === 'file' && (
          <Button variant="primary" onClick={() => downloadNode(node).catch(setError)}>
            {t('common.download')}
          </Button>
        )}
        {node.kind === 'file' && <Button onClick={() => previewNode(node).catch(setError)}>{t('details.preview')}</Button>}
        {node.kind === 'file' && <Button onClick={props.onShowVersions}>{t('details.versions')}</Button>}
        {node.kind === 'file' && canEdit && (
          <>
            <Button onClick={() => versionInput.current?.click()}>{t('details.uploadVersion')}</Button>
            <FileInput
              ref={versionInput}
              label={t('details.uploadVersion.label', { name: node.name })}
              onChange={(e) => {
                const file = e.target.files?.[0]
                e.target.value = ''
                if (file) void chooseVersion(file)
              }}
            />
          </>
        )}
        <Button onClick={() => setDialog('share')}>{t('details.share')}</Button>
        {canEdit && <Button onClick={() => setDialog('rename')}>{t('details.rename')}</Button>}
        {canEdit && <Button onClick={props.onMove}>{t('files.move')}</Button>}
        {canEdit && <Button onClick={props.onTrash}>{t('files.toTrash')}</Button>}
      </Actions>
      <NodeTags node={node} canEdit={canEdit} />

      {dialog === 'rename' && (
        <NameDialog
          open
          title={t('details.rename.title', { name: node.name })}
          label={t('common.name')}
          initial={node.name}
          selectBase={node.kind === 'file'}
          submitLabel={t('details.rename')}
          onClose={() => setDialog(null)}
          onSubmit={async (name) => {
            await unwrap(client.POST('/v1/nodes/{node_id}/rename', { params: { path: { node_id: node.id } }, body: { name } }))
            toast(t('details.renamed', { name }))
            props.onChanged()
          }}
        />
      )}
      {dialog === 'share' && <ShareDialog node={node} open onClose={() => setDialog(null)} onChanged={props.onChanged} />}
      {versionFile && (
        <KeepVersionDialog
          node={node}
          previousAt={versionFile.previousAt}
          onClose={() => setVersionFile(null)}
          onUpload={async (name) => {
            if (name) await nameHeadVersion(node, name)
            uploadVersion(versionFile.file, name ? undefined : versionFile.previousAt)
            setVersionFile(null)
          }}
        />
      )}
    </Stack>
  )
}

/**
 * Asked before "Upload new version" would replace the person's own save of the last 10 minutes:
 * a name keeps that save as its own version (named versions never join, design 5.3).
 */
function KeepVersionDialog(props: { node: Node; previousAt: string; onClose: () => void; onUpload: (name: string) => Promise<void> }) {
  const [name, setName] = useState('')
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  return (
    <Dialog title={t('keepVersion.title')} open onClose={props.onClose}>
      <Stack
        as="form"
        gap={4}
        onSubmit={async (e) => {
          e.preventDefault()
          setBusy(true)
          setError(undefined)
          try {
            await props.onUpload(name.trim())
          } catch (err) {
            setError(err)
            setBusy(false)
          }
        }}
      >
        <p>{t('keepVersion.body', { name: props.node.name, when: formatWhen(props.previousAt).toLowerCase() })}</p>
        <Field label={t('keepVersion.name')}>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} placeholder={t('keepVersion.placeholder')} />
        </Field>
        <ErrorNote error={error} />
        <Actions>
          <Button onClick={props.onClose}>{t('common.cancel')}</Button>
          <Button type="submit" variant="primary" disabled={busy} busy={busy}>
            {name.trim() ? t('keepVersion.keep') : t('keepVersion.replace')}
          </Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}

function NodeTags({ node, canEdit }: { node: Node; canEdit: boolean }) {
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const [pick, setPick] = useState('')
  const [creating, setCreating] = useState(false)
  const nodeTags = useApi(() => unwrap(client.GET('/v1/nodes/{node_id}/tags', { params: { path: { node_id: node.id } } })), [node.id])
  const allTags = useApi(() => unwrap(client.GET('/v1/tags')), [])

  async function attach(tagId: string, tagName?: string) {
    setError(undefined)
    try {
      if (tagId === '__new') {
        setCreating(true)
        return
      }
      await unwrap(client.PUT('/v1/nodes/{node_id}/tags/{tag_id}', { params: { path: { node_id: node.id, tag_id: tagId } } }))
      const name = tagName ?? allTags.data?.items.find((tag) => tag.id === tagId)?.name
      if (name) toast(t('tags.added', { name }))
      nodeTags.reload()
    } catch (e) {
      setError(e)
    } finally {
      setPick('')
    }
  }

  async function detach(tagId: string) {
    setError(undefined)
    try {
      await unwrap(client.DELETE('/v1/nodes/{node_id}/tags/{tag_id}', { params: { path: { node_id: node.id, tag_id: tagId } } }))
      nodeTags.reload()
    } catch (e) {
      setError(e)
    }
  }

  return (
    <Stack as="section" aria-labelledby="tags-title">
      <h3 id="tags-title">{t('tags.title')}</h3>
      <ErrorNote error={error} />
      <Loaded state={nodeTags}>
        {({ items }) => (
          <TagList>
            {items.length === 0 && <ListEmpty>{t('tags.none')}</ListEmpty>}
            {items.map((tag) => (
              <Tag key={tag.id} name={tag.name} color={tag.color} onRemove={canEdit ? () => detach(tag.id) : undefined} />
            ))}
          </TagList>
        )}
      </Loaded>
      {canEdit && allTags.data && (
        <Field label={t('tags.add')}>
          <Select value={pick} onChange={(e) => attach(e.target.value)}>
            <option value="">{t('tags.choose')}</option>
            {allTags.data.items
              .filter((tag) => !nodeTags.data?.items.some((x) => x.id === tag.id))
              .map((tag) => (
                <option key={tag.id} value={tag.id}>
                  {tag.name}
                </option>
              ))}
            <option value="__new">{t('tags.newOption')}</option>
          </Select>
        </Field>
      )}
      {creating && (
        <NameDialog
          open
          title={t('tags.new')}
          label={t('tags.name')}
          initial=""
          placeholder={t('tags.placeholder')}
          submitLabel={t('tags.createAdd')}
          onClose={() => setCreating(false)}
          onSubmit={async (name) => {
            const res = await unwrap(client.POST('/v1/tags', { body: { name } }))
            allTags.reload()
            await attach(res.tag.id, res.tag.name)
          }}
        />
      )}
    </Stack>
  )
}
