import { useEffect, useRef, useState } from 'react'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { Actor } from '../../app/names'
import { fileTypeLabel, formatBytes, formatDate, formatWhen, roleLabels } from '../../components/format'
import {
  Actions,
  Button,
  ErrorNote,
  Fact,
  Facts,
  Field,
  FileInput,
  IconButton,
  ListEmpty,
  Loaded,
  Menu,
  Select,
  Stack,
  Tag,
  TagList,
  Toolbar,
  useToast,
  type MenuItem,
} from '../../ui'
import { downloadNode, newVersionTarget, previewNode, type Node } from './api'
import { NameDialog } from './dialogs'
import { ShareDialog } from './ShareDialog'
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
  const { toast } = useToast()
  const uploads = useUploads()
  const [dialog, setDialog] = useState<'rename' | 'share' | null>(null)
  const [error, setError] = useState<unknown>()
  const canEdit = node.my_role === 'edit' || node.my_role === 'manage'
  const heading = useRef<HTMLHeadingElement>(null)
  const versionInput = useRef<HTMLInputElement>(null)

  // Move the focus to the panel when it opens, so keyboard and phone users land on it.
  useEffect(() => {
    heading.current?.focus()
    heading.current?.scrollIntoView?.({ block: 'start', behavior: 'smooth' })
  }, [node.id])

  /**
   * "Upload new version" is an explicit save: it always makes a new version (`coalesce: false`,
   * C-8, decision 0003-6), so the content before it stays in Versions.
   */
  function uploadVersion(file: File) {
    setError(undefined)
    uploads.add([{ file, target: newVersionTarget(node) }], node.tenant_id)
  }

  const more: MenuItem[] = [
    ...(node.kind === 'file' ? [{ label: t('details.versions'), onSelect: props.onShowVersions }] : []),
    ...(node.kind === 'file' && canEdit ? [{ label: t('details.uploadVersion'), onSelect: () => versionInput.current?.click() }] : []),
    ...(canEdit
      ? [
          { label: t('details.rename'), onSelect: () => setDialog('rename') },
          { label: t('files.move'), onSelect: props.onMove },
          { label: t('files.toTrash'), onSelect: props.onTrash, danger: true },
        ]
      : []),
  ]

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
      {/* The main actions as buttons, the rest in the row menu (design-system 4.4, 4.7). */}
      <Actions align="start">
        {node.kind === 'file' && (
          <Button variant="primary" onClick={() => downloadNode(node).catch(setError)}>
            {t('common.download')}
          </Button>
        )}
        {node.kind === 'file' && <Button onClick={() => previewNode(node).catch(setError)}>{t('details.preview')}</Button>}
        <Button onClick={() => setDialog('share')}>{t('details.share')}</Button>
        {more.length > 0 && <Menu label={t('details.more')} context={t('details.more.of', { name: node.name })} items={more} align="end" />}
      </Actions>
      {node.kind === 'file' && canEdit && (
        <FileInput
          ref={versionInput}
          label={t('details.uploadVersion.label', { name: node.name })}
          onChange={(e) => {
            const file = e.target.files?.[0]
            e.target.value = ''
            if (file) uploadVersion(file)
          }}
        />
      )}
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
    </Stack>
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
