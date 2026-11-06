import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { roleLabels } from '../../components/format'
import { Dialog } from '../../components/ui'
import { FolderPicker } from '../files/dialogs'

const roles: Schemas['Role'][] = ['view', 'comment', 'edit', 'manage']

/**
 * Form fields of an `AgentScope`: where, the highest role, the end date and the changes per hour.
 * `subtree: ''` means "only in a folder" with no folder chosen yet; the form must not send it.
 */
export function ScopeFields(props: {
  /** The tenant of the agent. Calls send it explicitly (the consent page is outside a tenant). */
  tenantId: string
  scope: Schemas['AgentScope']
  onChange: (scope: Schemas['AgentScope']) => void
  idPrefix: string
  /** The name of the space, for "Everywhere you have access in <space>". */
  spaceName?: string
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
      <p className="muted small">An agent never gets more access than you have.</p>
      <fieldset className="stack plain">
        <legend>Where</legend>
        <label className="check">
          <input
            type="radio"
            name={`${idPrefix}-where`}
            checked={typeof scope.subtree === 'string'}
            onChange={() => onChange({ ...scope, subtree: scope.subtree ?? '' })}
          />
          Only in a folder
        </label>
        {typeof scope.subtree === 'string' && (
          <div className="row indent">
            <span className="grow">{scope.subtree ? (subtree.data ? `Folder ${subtree.data.name}` : 'A folder') : 'No folder chosen yet'}</span>
            <button type="button" onClick={() => setPicking(true)}>
              Choose folder
            </button>
          </div>
        )}
        <label className="check">
          <input type="radio" name={`${idPrefix}-where`} checked={typeof scope.subtree !== 'string'} onChange={() => onChange({ ...scope, subtree: null })} />
          Everywhere you have access in {props.spaceName ?? 'this space'}
        </label>
      </fieldset>
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
      <label>
        Ends on (optional)
        <input
          type="date"
          value={expires}
          onChange={(e) =>
            onChange({ ...scope, expires_at: e.target.value ? new Date(`${e.target.value}T23:59:59Z`).toISOString() : null })
          }
        />
      </label>
      <label>
        Changes per hour, at most (optional)
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
            startId={scope.subtree || undefined}
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
