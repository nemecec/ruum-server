import { useState } from 'react'
import { Link } from 'react-router'
import type { Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { actorLabel, useNames } from '../../app/useNames'
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

/**
 * "Shared with me": the items that other people shared with the person, across all spaces where
 * they are a member or guest. Each item belongs to its own space (`node.tenant_id`): its link and
 * its download use that space, not the space of the page.
 */
export function SharedPage() {
  const shared = usePagedApi(listSharedWithMe, [])
  return (
    <PageBody>
      <h1>Shared with me</h1>
      <Loaded state={shared}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title="Nothing shared with you yet" text="When someone shares a file or folder with you, it shows up here." />
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
  const spaceName = (id: string) => tenants.find((t) => t.tenant.id === id)?.tenant.name ?? 'A shared space'
  return (
    <>
      <ErrorNote error={error} />
      <Table caption="Shared items">
        <THead>
          <Tr>
            <Th scope="col">Name</Th>
            {showSpace && <Th scope="col">Space</Th>}
            <Th scope="col">Your access</Th>
            <Th scope="col" hideNarrow>
              Shared by
            </Th>
            <Th scope="col" hideNarrow>
              Shared
            </Th>
            <Th scope="col">
              <VisuallyHidden>Actions</VisuallyHidden>
            </Th>
          </Tr>
        </THead>
        <TBody>
          {items.map(({ node, role, shared_by, shared_at }) => {
            const by = actorLabel(names, shared_by.actor, shared_by.on_behalf_of, shared_by.on_behalf_of_display_name)
            const sharer = by.text === 'Someone' ? `Someone in ${spaceName(node.tenant_id)}` : by.text
            return (
              <Tr key={node.id}>
                <Th scope="row">
                  <NameCell icon={<KindIcon kind={node.kind} type={node.type} name={node.name} />}>
                    <Link to={sharedItemPath(node)}>{node.name}</Link>
                  </NameCell>
                </Th>
                {showSpace && <Td>{spaceName(node.tenant_id)}</Td>}
                <Td>{roleLabels[role]}</Td>
                <Td hideNarrow>{sharer}</Td>
                <Td hideNarrow>
                  <span title={formatDate(shared_at)}>{formatWhen(shared_at)}</span>
                </Td>
                <Td actions>
                  {node.kind === 'file' && (
                    <Button
                      size="sm"
                      aria-label={`Download ${node.name}`}
                      onClick={() => {
                        setError(undefined)
                        downloadNode(node).catch(setError)
                      }}
                    >
                      Download
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
