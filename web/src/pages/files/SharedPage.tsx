import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import type { Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { actorLabel, useNames } from '../../app/useNames'
import { rememberSpaceNames } from '../../app/sharedSpaces'
import { useSession } from '../../app/useSession'
import { formatDate, formatWhen, roleLabels } from '../../components/format'
import {
  Button,
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
  Th,
  THead,
  Tr,
  VisuallyHidden,
} from '../../ui'
import { downloadNode, listSharedWithMe, sharedItemPath } from './api'
import { t } from '../../i18n'

/**
 * "Shared with me": the items that other people shared with the person, across all spaces where
 * they are a member or guest. Each item belongs to its own space (`node.tenant_id`): its link and
 * its download use that space, not the space of the page.
 */
export function SharedPage() {
  const shared = usePagedApi(listSharedWithMe, [])
  // The shell names a space that is not in the person's list from these (C-10).
  useEffect(() => {
    if (shared.data) rememberSpaceNames(shared.data.items)
  }, [shared.data])
  return (
    <PageBody>
      <h1>{t('shell.nav.shared')}</h1>
      <Loaded state={shared}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title={t('shared.empty.title')} text={t('shared.empty.text')} />
          ) : (
            <SharedTable items={items} showSpace />
          )
        }
      </Loaded>
      <LoadMore state={shared} />
    </PageBody>
  )
}

/** A table of shared items. Every item links to a page that works with the person's grant. */
export function SharedTable({ items, showSpace }: { items: Schemas['SharedItem'][]; showSpace?: boolean }) {
  const { tenants } = useSession()
  const names = useNames()
  const [error, setError] = useState<unknown>()
  // Every item names its space (C-10, decision 0003-6), also a space the person is only a guest of.
  const spaceName = (item: Schemas['SharedItem']) =>
    item.tenant_name || (tenants.find((m) => m.tenant.id === item.node.tenant_id)?.tenant.name ?? t('shell.sharedSpace'))
  return (
    <>
      <ErrorNote error={error} />
      <Table caption={t('shared.caption')}>
        <THead>
          <Tr>
            <Th scope="col">{t('common.name')}</Th>
            {showSpace && (
              <Th scope="col" hideNarrow>
                {t('shell.space')}
              </Th>
            )}
            <Th scope="col" hideNarrow>
              {t('shared.access')}
            </Th>
            <Th scope="col" hideNarrow>
              {t('shared.by')}
            </Th>
            <Th scope="col" hideNarrow>
              {t('shared.when')}
            </Th>
            <Th scope="col" hideNarrow>
              <VisuallyHidden>{t('common.actions')}</VisuallyHidden>
            </Th>
          </Tr>
        </THead>
        <TBody>
          {items.map((item) => {
            const { node, role, shared_by, shared_at } = item
            const by = actorLabel(names, shared_by.actor, shared_by.on_behalf_of, shared_by.on_behalf_of_display_name)
            const sharer = by.text === t('actor.someone') ? t('shared.someoneIn', { space: spaceName(item) }) : by.text
            return (
              <Tr key={node.id}>
                <Th scope="row">
                  <NameCell
                    icon={<KindIcon kind={node.kind} type={node.type} name={node.name} />}
                    meta={
                      // On the phone the other columns are hidden; the row opens the item, which has Download.
                      showSpace
                        ? t('shared.meta.space', { space: spaceName(item), access: roleLabels[role], who: sharer })
                        : t('shared.meta', { access: roleLabels[role], who: sharer })
                    }
                  >
                    <Link to={sharedItemPath(node)}>{node.name}</Link>
                  </NameCell>
                </Th>
                {showSpace && <Td hideNarrow>{spaceName(item)}</Td>}
                <Td hideNarrow>{roleLabels[role]}</Td>
                <Td hideNarrow>{sharer}</Td>
                <Td hideNarrow>
                  <span title={formatDate(shared_at)}>{formatWhen(shared_at)}</span>
                </Td>
                <Td actions hideNarrow>
                  {node.kind === 'file' && (
                    <Button
                      size="sm"
                      aria-label={t('common.download.named', { name: node.name })}
                      onClick={() => {
                        setError(undefined)
                        downloadNode(node).catch(setError)
                      }}
                    >
                      {t('common.download')}
                    </Button>
                  )}
                </Td>
              </Tr>
            )
          })}
        </TBody>
      </Table>
    </>
  )
}
