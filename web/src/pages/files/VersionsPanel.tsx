import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { Actor } from '../../app/names'
import { formatBytes, formatDate, formatShortDate, formatTime, whenParts } from '../../components/format'
import {
  Actions,
  Badge,
  Button,
  ConfirmButton,
  DiffChange,
  DiffList,
  DiffView,
  Dialog,
  EmptyState,
  ErrorNote,
  Field,
  List,
  ListRow,
  Menu,
  Loaded,
  Loading,
  LoadMore,
  Row,
  Select,
  Stack,
  Text,
  Toolbar,
  useToast,
} from '../../ui'
import { downloadNode, type Node } from './api'
import { NameDialog } from './dialogs'
import { t, type MessageKey } from '../../i18n'

/** Version history of a file: download, name, compare, restore (journeys J4). */
export function VersionsPanel(props: { node: Node; onClose: () => void; onRestored: () => void }) {
  const { node } = props
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const [compare, setCompare] = useState<Schemas['Version'] | null>(null)
  const versions = usePagedApi(
    (cursor) =>
      unwrap(client.GET('/v1/nodes/{node_id}/versions', { params: { path: { node_id: node.id }, query: cursor ? { cursor } : {} } })),
    [node.id, node.head_version],
  )
  const canEdit = node.my_role === 'edit' || node.my_role === 'manage'
  const [naming, setNaming] = useState<Schemas['Version'] | null>(null)

  async function saveName(v: Schemas['Version'], label: string) {
    await unwrap(
      client.PUT('/v1/nodes/{node_id}/versions/{version_id}/label', {
        params: { path: { node_id: node.id, version_id: v.id } },
        body: { label: label || null },
      }),
    )
    toast(label ? t('versions.named', { label }) : t('versions.unnamed'))
    versions.reload()
  }

  async function restore(v: Schemas['Version']) {
    const before = node.head_version
    await unwrap(
      client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', { params: { path: { node_id: node.id, version_id: v.id } } }),
    )
    setCompare(null)
    props.onRestored()
    toast(t('versions.restored', { when: formatShortDate(v.created_at) }), {
      undo: before
        ? async () => {
            await unwrap(
              client.POST('/v1/nodes/{node_id}/versions/{version_id}/restore', {
                params: { path: { node_id: node.id, version_id: before } },
              }),
            )
            props.onRestored()
            return t('versions.restored.undone')
          }
        : undefined,
    })
  }

  return (
    <Stack as="section" gap={4} aria-labelledby="versions-title">
      <Toolbar>
        <h2 id="versions-title">{t('versions.title', { name: node.name })}</h2>
        <Button size="sm" onClick={props.onClose}>
          {t('versions.back')}
        </Button>
      </Toolbar>
      <ErrorNote error={error} />
      {naming && (
        <NameDialog
          open
          title={t('versions.name.title')}
          help={t('versions.name.help')}
          label={t('versions.name.label')}
          placeholder={t('versions.name.placeholder')}
          initial={naming.label ?? ''}
          submitLabel={t('versions.name.save')}
          allowEmpty
          onClose={() => setNaming(null)}
          onSubmit={(label) => saveName(naming, label)}
        />
      )}
      <Loaded state={versions}>
        {(page) => {
          const byId = new Map(page.items.map((v) => [v.id, v]))
          return (
            <>
              <List ordered>
                {page.items.map((v) => {
                  const current = v.id === node.head_version
                  const when = formatDate(v.created_at)
                  const source = v.restored_from ? byId.get(v.restored_from) : undefined
                  return (
                    <ListRow key={v.id} block current={current}>
                      <Row gap={2}>
                        <strong>{versionWhen(v.created_at)}</strong>
                        {current && <Badge tone="primary">{t('versions.current')}</Badge>}
                        {v.label && <Badge>{v.label}</Badge>}
                      </Row>
                      <Text as="div" tone="muted" size="sm">
                        <Actor pair={v} /> {t('versions.size', { size: formatBytes(v.size_bytes) })}
                      </Text>
                      {v.restored_from && (
                        <Text as="div" tone="muted" size="sm">
                          {source ? t('versions.restoredFrom', { when: formatShortDate(source.created_at) }) : t('versions.restoredFrom.older')}
                        </Text>
                      )}
                      <Actions align="start">
                        {canEdit && !current && (
                          <ConfirmButton
                            size="sm"
                            label={t('trash.restore')}
                            ariaLabel={t('versions.restore.label', { when })}
                            title={t('versions.restore.title', { when: formatShortDate(v.created_at) })}
                            message={t('versions.restore.message')}
                            confirmLabel={t('trash.restore')}
                            onConfirm={() => restore(v)}
                          />
                        )}
                        {page.items.length > 1 && (
                          <Button size="sm" aria-label={t('versions.compare.label', { when })} onClick={() => setCompare(v)}>
                            {t('versions.compare')}
                          </Button>
                        )}
                        <Menu
                          label={t('details.more')}
                          context={t('versions.more.of', { when })}
                          size="sm"
                          items={[
                            { label: t('common.download'), onSelect: () => void downloadNode(node, v.id).catch(setError) },
                            ...(canEdit ? [{ label: v.label ? t('versions.rename') : t('versions.name'), onSelect: () => setNaming(v) }] : []),
                          ]}
                        />
                      </Actions>
                    </ListRow>
                  )
                })}
              </List>
              {page.items.length === 1 && !page.nextCursor && (
                <EmptyState title={t('versions.only.title')} text={t('versions.only.text')} />
              )}
              <Text tone="muted" size="sm">
                {t('versions.retention')}
              </Text>
              {compare && <CompareDialog node={node} from={compare} versions={page.items} onClose={() => setCompare(null)} />}
            </>
          )
        }}
      </Loaded>
      <LoadMore state={versions} />
    </Stack>
  )
}

const kindLabels: Record<Schemas['DiffChangeKind'], MessageKey> = {
  insert: 'diff.kind.insert',
  delete: 'diff.kind.delete',
  modify: 'diff.kind.modify',
  move: 'diff.kind.move',
  format: 'diff.kind.format',
  replace: 'diff.kind.replace',
}

/**
 * When a version was made, always with the time of day, so "yesterday's" version is easy to find:
 * "13 minutes ago (15:13)", "Yesterday at 18:20", "20 Oct 2026, 14:00".
 */
function versionWhen(iso: string): string {
  const { text, kind } = whenParts(iso)
  if (kind === 'recent') return t('versions.when.recent', { when: text, time: formatTime(iso) })
  if (kind === 'day') return text
  return formatDate(iso)
}

/** "1 change", "3 changes": the plain summary of a comparison (the counts by kind confused people). */
function changeCount(s: { inserted: number; deleted: number; modified: number; moved?: number | null }): string {
  const n = s.inserted + s.deleted + s.modified + (s.moved ?? 0)
  return t('diff.count', { n })
}

/** Plain names of the diff plugins (ia.md: "Compare as" text, data, image, file). */
function pluginLabel(id: string, inline = false): string {
  const name = id.split('@')[0]
  const labels: Record<string, MessageKey> = {
    text: 'diff.plugin.text',
    json: 'diff.plugin.json',
    xml: 'diff.plugin.xml',
    image: 'diff.plugin.image',
    binary: 'diff.plugin.binary',
  }
  const inlineLabels: Record<string, MessageKey> = {
    text: 'diff.plugin.text.inline',
    json: 'diff.plugin.json.inline',
    xml: 'diff.plugin.xml.inline',
    image: 'diff.plugin.image.inline',
    binary: 'diff.plugin.binary.inline',
  }
  // A plugin the UI does not know: its ID as the server sends it.
  return (inline ? inlineLabels : labels)[name] ? t((inline ? inlineLabels : labels)[name]) : name
}

/** A change path in plain words: `lines/2` is "Line 2". */
function pathLabel(c: Schemas['DiffChange']): string | null {
  if (c.label) return c.label
  if (!c.path) return null
  const line = /^lines?\/(\d+)$/.exec(c.path)
  if (line) return t('diff.line', { n: line[1] })
  return c.path.replace(/\//g, ' > ')
}

/**
 * The changes between two versions (`diffVersions`) in a wide dialog (a full page on the phone),
 * in the one change model of all diff plugins (design 5.4).
 */
function CompareDialog(props: { node: Node; from: Schemas['Version']; versions: Schemas['Version'][]; onClose: () => void }) {
  const { node, from } = props
  const others = props.versions.filter((v) => v.id !== from.id)
  const [to, setTo] = useState(from.id === node.head_version ? (others[0]?.id ?? '') : '')
  const [plugin, setPlugin] = useState('')
  // "Compare as" is for people who know the plugins; it waits behind "More options".
  const [advanced, setAdvanced] = useState(false)
  const plugins = useApi(
    () => unwrap(client.GET('/v1/nodes/{node_id}/diff/plugins', { params: { path: { node_id: node.id } } })),
    [node.id],
  )
  // `to` empty means the current version. When `from` is the current version, compare an older
  // one to it instead (older first).
  const swap = from.id === node.head_version
  const query = swap ? { from: to, to: from.id } : { from: from.id, ...(to ? { to } : {}) }
  const diff = useApi(
    () =>
      query.from
        ? unwrap(
            client.GET('/v1/nodes/{node_id}/diff', {
              params: { path: { node_id: node.id }, query: { ...query, ...(plugin ? { plugin } : {}) } },
            }),
          )
        : Promise.resolve(null),
    [node.id, query.from, query.to, plugin],
  )
  const pluginItems = plugins.data?.items ?? []
  const label = (v: Schemas['Version']) =>
    t('compare.version', {
      when: formatShortDate(v.created_at),
      current: v.id === node.head_version ? 'yes' : 'no',
      label: v.label ?? '',
      named: v.label ? 'yes' : 'no',
    })

  return (
    <Dialog title={t('compare.title')} open onClose={props.onClose} size="full">
      <Stack gap={4}>
        <Row gap={4} align="end">
          <Text>
            <Text as="span" tone="muted">
              {swap ? t('compare.to') : t('compare.from')}
            </Text>{' '}
            <strong>{label(from)}</strong>
          </Text>
          <Field label={swap ? t('compare.from') : t('compare.to')}>
            <Select value={to} onChange={(e) => setTo(e.target.value)}>
              {!swap && <option value="">{t('compare.current')}</option>}
              {others
                .filter((v) => swap || v.id !== node.head_version)
                .map((v) => (
                  <option key={v.id} value={v.id}>
                    {label(v)}
                  </option>
                ))}
            </Select>
          </Field>
          {pluginItems.length > 1 && !advanced && (
            <Button variant="link" onClick={() => setAdvanced(true)}>
              {t('compare.more')}
            </Button>
          )}
          {pluginItems.length > 1 && advanced && (
            <Field label={t('compare.as')}>
              <Select value={plugin} onChange={(e) => setPlugin(e.target.value)}>
                <option value="">{t('compare.best', { plugin: pluginLabel(pluginItems[0].name) })}</option>
                {pluginItems.map((p) => (
                  <option key={p.name} value={p.name}>
                    {pluginLabel(p.name)}
                  </option>
                ))}
              </Select>
            </Field>
          )}
        </Row>
        {diff.loading && !diff.data ? (
          <Loading label={t('compare.loading')} />
        ) : (
          <Loaded state={diff}>
            {(d) =>
              !d ? null : (
                <DiffView busy={diff.loading}>
                  {d.changes.length === 0 ? (
                    <p>{t('compare.same')}</p>
                  ) : (
                    <>
                      <p>{changeCount(d.summary)}</p>
                      <ChangeList changes={d.changes} />
                    </>
                  )}
                  {d.render?.html && (
                    <p>
                      <a href={d.render.html} target="_blank" rel="noopener noreferrer">
                        {t('compare.fullView')}
                      </a>
                    </p>
                  )}
                  {d.truncated && <Text tone="muted">{t('compare.truncated')}</Text>}
                  <Text tone="muted" size="sm">
                    {t('compare.comparedAs', { plugin: pluginLabel(d.plugin, true) })}
                  </Text>
                </DiffView>
              )
            }
          </Loaded>
        )}
        <Actions>
          <Button onClick={props.onClose}>{t('ui.close')}</Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}

function ChangeList({ changes }: { changes: Schemas['DiffChange'][] }) {
  return (
    <DiffList>
      {changes.map((c, i) => (
        <DiffChange key={`${c.path ?? ''}-${i}`} kind={t(kindLabels[c.kind])} where={pathLabel(c)} old={c.old} new={c.new} format={c.change}>
          {c.detail && c.detail.length > 0 && <ChangeList changes={c.detail} />}
        </DiffChange>
      ))}
    </DiffList>
  )
}
