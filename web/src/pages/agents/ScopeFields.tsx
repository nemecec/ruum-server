import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { roleLabels } from '../../components/format'
import { Dialog } from '../../components/ui'
import { FolderPicker } from '../files/dialogs'

const roles: Schemas['Role'][] = ['view', 'comment', 'edit', 'manage']

/** Form fields of an `AgentScope`: the folder, the highest role and the end date. */
export function ScopeFields(props: {
  /** The tenant of the agent. Calls send it explicitly (the consent page is outside a tenant). */
  tenantId: string
  scope: Schemas['AgentScope']
  onChange: (scope: Schemas['AgentScope']) => void
  idPrefix: string
}) {
  const { scope, onChange, idPrefix, tenantId } = props
  const [picking, setPicking] = useState(false)
  const subtree = useApi(
    () =>
      scope.subtree
        ? unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: scope.subtree }, header: { 'Ruum-Tenant': tenantId } } }))
        : Promise.resolve(null),
    [scope.subtree, tenantId],
  )
  const expires = scope.expires_at ? scope.expires_at.slice(0, 10) : ''

  return (
    <fieldset className="stack">
      <legend>What the agent may do</legend>
      <div>
        <span id={`${idPrefix}-where`}>Where</span>
        <div className="row" aria-labelledby={`${idPrefix}-where`}>
          <span className="grow">
            {scope.subtree ? (subtree.data ? `Folder ${subtree.data.name}` : 'A folder') : 'Everything I can access in this tenant'}
          </span>
          <button type="button" onClick={() => setPicking(true)}>
            Choose folder
          </button>
          {scope.subtree && (
            <button type="button" onClick={() => onChange({ ...scope, subtree: null })}>
              Everything
            </button>
          )}
        </div>
      </div>
      <label>
        Highest role
        <select value={scope.max_role} onChange={(e) => onChange({ ...scope, max_role: e.target.value as Schemas['Role'] })}>
          {roles.map((r) => (
            <option key={r} value={r}>
              {roleLabels[r]}
            </option>
          ))}
        </select>
      </label>
      <p className="muted small">The agent never gets more than you have yourself.</p>
      <label>
        Access ends on (optional)
        <input
          type="date"
          value={expires}
          onChange={(e) =>
            onChange({ ...scope, expires_at: e.target.value ? new Date(`${e.target.value}T23:59:59Z`).toISOString() : null })
          }
        />
      </label>
      <label>
        Writes per hour at most (optional)
        <input
          type="number"
          min={1}
          value={scope.budget?.max_writes_per_hour ?? ''}
          onChange={(e) =>
            onChange({
              ...scope,
              budget: { ...scope.budget, max_writes_per_hour: e.target.value ? Number(e.target.value) : null },
            })
          }
        />
      </label>
      {picking && (
        <Dialog title="Choose a folder for the agent" open onClose={() => setPicking(false)}>
          <FolderPicker
            tenantId={tenantId}
            startId={scope.subtree ?? undefined}
            pickLabel="Use this folder"
            onPick={(folder) => {
              onChange({ ...scope, subtree: folder.parent_id ? folder.id : null })
              setPicking(false)
            }}
          />
        </Dialog>
      )}
    </fieldset>
  )
}
