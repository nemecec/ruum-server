import { Link, Navigate, useNavigate, useParams } from 'react-router'
import { client, RuumApiError, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { tenantPath, useTenant } from '../../app/tenant'
import { EmptyState, Loaded, PageBody, useToast } from '../../ui'
import { trashNodes } from './api'
import { NodeDetails } from './NodeDetails'
import { VersionsPanel } from './VersionsPanel'
import { useState } from 'react'
import { t } from '../../i18n'

/**
 * One item by its ID (`/t/<space>/item/<id>`): the page for a file that was shared alone, whose
 * folder the person cannot open. A folder goes to the file browser.
 */
export function ItemPage() {
  const { nodeId = '' } = useParams()
  const { tenant, listed } = useTenant()
  const { toast } = useToast()
  const navigate = useNavigate()
  const [versions, setVersions] = useState(false)
  const node = useApi(() => unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: nodeId } } })), [nodeId])

  if (node.error instanceof RuumApiError && node.error.status === 404 && listed === false) return <NotInSpaceYet />
  return (
    <PageBody narrow>
      <p>
        <Link to={tenantPath(tenant.id, 'shared')}>{t('shell.nav.shared')}</Link>
      </p>
      <Loaded state={node}>
        {(n) =>
          n.kind === 'folder' ? (
            <Navigate to={tenantPath(n.tenant_id, 'files', n.id)} replace />
          ) : versions ? (
            <VersionsPanel node={n} onClose={() => setVersions(false)} onRestored={node.reload} />
          ) : (
            <NodeDetails
              node={n}
              onChanged={node.reload}
              onShowVersions={() => setVersions(true)}
              onMove={() =>
                navigate(n.parent_id ? `${tenantPath(n.tenant_id, 'files', n.parent_id)}?node=${n.id}` : tenantPath(n.tenant_id, 'files'))
              }
              onTrash={async () => {
                const { undo } = await trashNodes([n])
                toast(t('files.trashed.one', { name: n.name }), {
                  undo: async () => {
                    await undo()
                    node.reload()
                    return t('files.restored.one', { name: n.name })
                  },
                })
                navigate(tenantPath(n.tenant_id, 'shared'))
              }}
            />
          )
        }
      </Loaded>
    </PageBody>
  )
}

/**
 * An item of a space that is not in the person's list answers `404`: the share reached the
 * person, but the server has not made them a guest of that space (docs/TESTING.md, known gaps).
 */
export function NotInSpaceYet() {
  return (
    <PageBody>
      <EmptyState
        title={t('item.notInSpace.title')}
        text={t('item.notInSpace.text')}
      >
        <Link to="/">{t('common.goToFiles')}</Link>
      </EmptyState>
    </PageBody>
  )
}
