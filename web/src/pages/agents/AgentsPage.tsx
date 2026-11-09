import { useState, type FormEvent } from 'react'
import { Link, useNavigate } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useNames } from '../../app/useNames'
import { tenantPath, useTenant } from '../../app/tenant'
import { roleLabels } from '../../components/format'
import { ScopeFields } from './ScopeFields'
import { agentStatus } from './status'
import {
  Actions,
  AgentName,
  Button,
  Dialog,
  EmptyState,
  ErrorNote,
  Field,
  Loaded,
  LoadMore,
  PageBody,
  Stack,
  Table,
  TBody,
  Td,
  Text,
  TextInput,
  Th,
  THead,
  Toolbar,
  Tr,
} from '../../ui'
import { t } from '../../i18n'

/** The agents of the person (all agents of the space for admins). */
export function AgentsPage() {
  const { tenant, isAdmin } = useTenant()
  const names = useNames()
  const navigate = useNavigate()
  const agents = usePagedApi((cursor) => unwrap(client.GET('/v1/agents', { params: { query: cursor ? { cursor } : {} } })), [])
  const connect = () => navigate(tenantPath(tenant.id, 'agents', 'connect'))

  return (
    <PageBody>
      <Toolbar>
        <h1>{t('shell.nav.agents')}</h1>
        <Button variant="primary" onClick={connect}>
          {t('files.welcome.connect')}
        </Button>
      </Toolbar>
      <Text tone="muted">
        {t('agents.intro')}
        {isAdmin && ` ${t('agents.intro.admin', { space: tenant.name, personal: tenant.kind === 'personal' ? 'yes' : 'no' })}`}
      </Text>
      <Loaded state={agents}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title={t('agents.none.title')} text={t('agents.none.text')}>
              <Button variant="primary" onClick={connect}>
                {t('files.welcome.connect')}
              </Button>
            </EmptyState>
          ) : (
            <Table caption={t('shell.nav.agents')}>
              <THead>
                <Tr>
                  <Th scope="col">{t('common.name')}</Th>
                  <Th scope="col">{t('agents.status')}</Th>
                  <Th hideNarrow scope="col">
                    {t('scope.role')}
                  </Th>
                  {isAdmin && (
                    <Th hideNarrow scope="col">
                      {t('agents.worksFor')}
                    </Th>
                  )}
                </Tr>
              </THead>
              <TBody>
                {items.map((a) => (
                  <Tr key={a.id}>
                    <Th scope="row">
                      <AgentName>
                        <Link to={tenantPath(tenant.id, 'agents', a.id)}>{a.name}</Link>
                      </AgentName>
                    </Th>
                    <Td>{agentStatus(a)}</Td>
                    <Td hideNarrow>{roleLabels[a.scope.max_role]}</Td>
                    {isAdmin && <Td hideNarrow>{names.person(a.owner_id)?.name ?? t('actor.someone')}</Td>}
                  </Tr>
                ))}
              </TBody>
            </Table>
          )
        }
      </Loaded>
      <LoadMore state={agents} />
    </PageBody>
  )
}

/** Creates an agent with its own keys (for scripts and the CLI). */
export function RegisterAgentDialog(props: { onClose: () => void; onCreated: (a: Schemas['Agent']) => void }) {
  const { tenant } = useTenant()
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [scope, setScope] = useState<Schemas['AgentScope']>({ subtree: null, max_role: 'edit', expires_at: null })
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(undefined)
    try {
      const agent = await unwrap(
        client.POST('/v1/agents', {
          body: { name: name.trim(), ...(description.trim() ? { description: description.trim() } : {}), scope },
        }),
      )
      props.onCreated(agent)
    } catch (err) {
      setError(err)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog title={t('agents.register.title')} open onClose={props.onClose} size="md">
      <Stack as="form" onSubmit={submit}>
        <Field label={t('common.name')}>
          <TextInput required value={name} onChange={(e) => setName(e.target.value)} placeholder={t('agents.register.placeholder')} />
        </Field>
        <Field label={t('agents.register.description')}>
          <TextInput value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <ScopeFields tenantId={tenant.id} spaceName={tenant.name} scope={scope} onChange={setScope} idPrefix="register" />
        <ErrorNote error={error} />
        <Actions>
          <Button onClick={props.onClose}>{t('common.cancel')}</Button>
          <Button variant="primary" type="submit" disabled={busy || !name.trim() || scope.subtree === ''}>
            {t('agents.register.submit')}
          </Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}
