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
        <h1>Agents</h1>
        <Button variant="primary" onClick={connect}>
          Connect an agent
        </Button>
      </Toolbar>
      <Text tone="muted">
        Agents work in Ruum for you, with the access you give them. Every change they make shows their name and yours.
        {isAdmin && ` As an admin, you see every agent in ${tenant.name}.`}
      </Text>
      <Loaded state={agents}>
        {({ items }) =>
          items.length === 0 ? (
            <EmptyState title="No agents yet" text="Connect Claude, ChatGPT or another AI app, or create an agent with a key for a script.">
              <Button variant="primary" onClick={connect}>
                Connect an agent
              </Button>
            </EmptyState>
          ) : (
            <Table caption="Agents">
              <THead>
                <Tr>
                  <Th scope="col">Name</Th>
                  <Th scope="col">Status</Th>
                  <Th hideNarrow scope="col">
                    Highest role
                  </Th>
                  {isAdmin && (
                    <Th hideNarrow scope="col">
                      Works for
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
                    {isAdmin && <Td hideNarrow>{names.person(a.owner_id)?.name ?? 'Someone'}</Td>}
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
    <Dialog title="Create an agent with a key" open onClose={props.onClose} size="md">
      <Stack as="form" onSubmit={submit}>
        <Field label="Name">
          <TextInput required value={name} onChange={(e) => setName(e.target.value)} placeholder="Backup script" />
        </Field>
        <Field label="Description (optional)">
          <TextInput value={description} onChange={(e) => setDescription(e.target.value)} />
        </Field>
        <ScopeFields tenantId={tenant.id} spaceName={tenant.name} scope={scope} onChange={setScope} idPrefix="register" />
        <ErrorNote error={error} />
        <Actions>
          <Button onClick={props.onClose}>Cancel</Button>
          <Button variant="primary" type="submit" disabled={busy || !name.trim() || scope.subtree === ''}>
            Create agent
          </Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}
