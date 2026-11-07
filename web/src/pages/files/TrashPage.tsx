import { useState } from 'react'
import { client, unwrap } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useTenant } from '../../app/tenant'
import { formatDate, formatWhen } from '../../components/format'
import {
  Button,
  ConfirmButton,
  EmptyState,
  ErrorNote,
  KindIcon,
  Loaded,
  LoadMore,
  NameCell,
  PageBody,
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
import { t } from '../../i18n'

/** The trash: restore items, delete them for good, or empty the trash (copy.md 3.7). */
export function TrashPage() {
  const { isAdmin, tenant } = useTenant()
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const trash = usePagedApi((cursor) => unwrap(client.GET('/v1/trash', { params: { query: cursor ? { cursor } : {} } })), [])

  async function restore(id: string, name: string) {
    setError(undefined)
    try {
      const res = await unwrap(
        client.POST('/v1/nodes/{node_id}/restore', { params: { path: { node_id: id } }, body: { on_conflict: 'rename' } }),
      )
      let folder = t('trash.itsFolder')
      if (res.node.parent_id) {
        try {
          const parent = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: res.node.parent_id } } }))
          folder = parent.parent_id ? parent.name : t('shell.nav.files')
        } catch {
          // The folder name is only for the toast.
        }
      }
      toast(
        res.node.name === name
          ? t('trash.restored', { name, folder })
          : t('trash.restored.renamed', { name, folder, newName: res.node.name }),
      )
      trash.reload()
    } catch (e) {
      setError(e)
    }
  }

  const n = trash.data?.items.length ?? 0
  return (
    <PageBody>
      <Toolbar>
        <h1>{t('shell.nav.trash')}</h1>
        {n > 0 && (
          <ConfirmButton
            label={t('trash.empty')}
            title={trash.data?.nextCursor ? t('trash.empty.titleAll') : t('trash.empty.title', { n })}
            message={t('trash.empty.message')}
            confirmLabel={trash.data?.nextCursor ? t('trash.empty.confirmAll') : t('trash.empty.confirm', { n })}
            danger
            onConfirm={async () => {
              const res = await unwrap(client.POST('/v1/trash/empty'))
              toast(t('trash.emptied', { n: res.purged }))
              trash.reload()
            }}
          />
        )}
      </Toolbar>
      <Text tone="muted">
        {t('trash.intro', { space: tenant.name })}
        {isAdmin ? ` ${t('trash.intro.admin')}` : ''}
      </Text>
      <ErrorNote error={error} />
      <Loaded state={trash}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title={t('trash.none.title')} text={t('trash.none.text')} />
          ) : (
            <Table caption={t('trash.caption')}>
              <THead>
                <Tr>
                  <Th scope="col">{t('common.name')}</Th>
                  <Th scope="col" hideNarrow>
                    {t('trash.when')}
                  </Th>
                  <Th scope="col">
                    <VisuallyHidden>{t('common.actions')}</VisuallyHidden>
                  </Th>
                </Tr>
              </THead>
              <TBody>
                {items.map((node) => (
                  <Tr key={node.id}>
                    <Th scope="row">
                      <NameCell icon={<KindIcon kind={node.kind} type={node.type} name={node.name} />}>{node.name}</NameCell>
                    </Th>
                    <Td hideNarrow>
                      <span title={formatDate(node.trashed_at)}>{formatWhen(node.trashed_at)}</span>
                    </Td>
                    <Td actions>
                      <Button size="sm" aria-label={t('trash.restore.named', { name: node.name })} onClick={() => restore(node.id, node.name)}>
                        {t('trash.restore')}
                      </Button>
                      <ConfirmButton
                        size="sm"
                        label={t('trash.purge')}
                        ariaLabel={t('trash.purge.named', { name: node.name })}
                        title={t('trash.purge.title', { name: node.name })}
                        message={t('trash.purge.message')}
                        confirmLabel={t('trash.purge')}
                        danger
                        onConfirm={async () => {
                          await unwrap(client.DELETE('/v1/nodes/{node_id}', { params: { path: { node_id: node.id } } }))
                          toast(t('trash.purged', { name: node.name }))
                          trash.reload()
                        }}
                      />
                    </Td>
                  </Tr>
                ))}
              </TBody>
            </Table>
          )
        }
      </Loaded>
      <LoadMore state={trash} />
    </PageBody>
  )
}
