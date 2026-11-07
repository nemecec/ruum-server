import { useState } from 'react'
import { client, unwrap } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useTenant } from '../../app/tenant'
import { count, formatDate, formatWhen } from '../../components/format'
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
      let folder = 'its folder'
      if (res.node.parent_id) {
        try {
          const parent = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: res.node.parent_id } } }))
          folder = parent.parent_id ? parent.name : 'Files'
        } catch {
          // The folder name is only for the toast.
        }
      }
      toast(res.node.name === name ? `Restored ${name} to ${folder}.` : `Restored ${name} to ${folder} as ${res.node.name}.`)
      trash.reload()
    } catch (e) {
      setError(e)
    }
  }

  const n = trash.data?.items.length ?? 0
  return (
    <PageBody>
      <Toolbar>
        <h1>Trash</h1>
        {n > 0 && (
          <ConfirmButton
            label="Empty trash"
            title={trash.data?.nextCursor ? 'Delete everything in the trash for good?' : `Delete ${count(n, 'item')} for good?`}
            message="Everything in the trash is deleted, with its versions. You can't undo this."
            confirmLabel={trash.data?.nextCursor ? 'Delete everything for good' : `Delete ${count(n, 'item')} for good`}
            danger
            onConfirm={async () => {
              const res = await unwrap(client.POST('/v1/trash/empty'))
              toast(`Deleted ${count(res.purged, 'item')} for good.`)
              trash.reload()
            }}
          />
        )}
      </Toolbar>
      <Text tone="muted">
        Items stay in the trash for the time the plan of {tenant.name} allows. Then Ruum deletes them for good.
        {isAdmin ? ' Admins can restore items for longer.' : ''}
      </Text>
      <ErrorNote error={error} />
      <Loaded state={trash}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title="The trash is empty" text="Items you move to the trash stay here for a while, so you can bring them back." />
          ) : (
            <Table caption="Items in the trash">
              <THead>
                <Tr>
                  <Th scope="col">Name</Th>
                  <Th scope="col" hideNarrow>
                    Moved to the trash
                  </Th>
                  <Th scope="col">
                    <VisuallyHidden>Actions</VisuallyHidden>
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
                      <Button size="sm" aria-label={`Restore ${node.name}`} onClick={() => restore(node.id, node.name)}>
                        Restore
                      </Button>
                      <ConfirmButton
                        size="sm"
                        label="Delete for good"
                        ariaLabel={`Delete for good: ${node.name}`}
                        title={`Delete ${node.name} for good?`}
                        message="You can't undo this. Its versions are deleted too."
                        confirmLabel="Delete for good"
                        danger
                        onConfirm={async () => {
                          await unwrap(client.DELETE('/v1/nodes/{node_id}', { params: { path: { node_id: node.id } } }))
                          toast(`Deleted ${node.name} for good.`)
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
