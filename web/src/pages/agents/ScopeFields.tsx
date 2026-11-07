import { useState } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { roleLabels } from '../../components/format'
import { FolderPicker } from '../files/dialogs'
import { Button, Dialog, Field, Fieldset, Grow, Indent, Radio, Row, Select, Text, TextInput } from '../../ui'
import { t } from '../../i18n'

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
    <Fieldset legend={t('scope.legend')}>
      <Text tone="muted" size="sm">
        {t('scope.never')}
      </Text>
      <Fieldset legend={t('scope.where')}>
        <Radio
          name={`${idPrefix}-where`}
          checked={typeof scope.subtree === 'string'}
          onChange={() => onChange({ ...scope, subtree: scope.subtree ?? '' })}
          label={t('scope.folder')}
        />
        {typeof scope.subtree === 'string' && (
          <Indent>
            <Row>
              <Grow>
                {scope.subtree
                  ? subtree.data
                    ? t('scope.folder.named', { name: subtree.data.name })
                    : t('scope.folder.unknown')
                  : t('scope.folder.none')}
              </Grow>
              <Button onClick={() => setPicking(true)}>{t('scope.folder.choose')}</Button>
            </Row>
          </Indent>
        )}
        <Radio
          name={`${idPrefix}-where`}
          checked={typeof scope.subtree !== 'string'}
          onChange={() => onChange({ ...scope, subtree: null })}
          label={props.spaceName ? t('scope.everywhere', { space: props.spaceName }) : t('scope.everywhere.here')}
        />
      </Fieldset>
      <Field label={t('scope.role')}>
        <Select value={scope.max_role} onChange={(e) => onChange({ ...scope, max_role: e.target.value as Schemas['Role'] })}>
          {roles.map((r) => (
            <option key={r} value={r}>
              {roleLabels[r]}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={t('scope.ends')}>
        <TextInput
          type="date"
          value={expires}
          onChange={(e) =>
            onChange({ ...scope, expires_at: e.target.value ? new Date(`${e.target.value}T23:59:59Z`).toISOString() : null })
          }
        />
      </Field>
      <Field label={t('scope.budget')}>
        <TextInput
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
      </Field>
      {picking && (
        <Dialog title={t('scope.picker.title')} open onClose={() => setPicking(false)}>
          <FolderPicker
            tenantId={tenantId}
            startId={scope.subtree || undefined}
            pickLabel={t('scope.picker.use')}
            onPick={(folder) => {
              onChange({ ...scope, subtree: folder.parent_id ? folder.id : null })
              setPicking(false)
            }}
          />
        </Dialog>
      )}
    </Fieldset>
  )
}
