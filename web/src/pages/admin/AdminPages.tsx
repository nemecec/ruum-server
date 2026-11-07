import { useState, type FormEvent } from 'react'
import { Link, Outlet } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { Actor } from '../../app/names'
import { useNames } from '../../app/useNames'
import { StartTeamDialog } from '../../app/Shell'
import { formatBytes, formatDate, formatWhen, memberRoleLabels, memberRoleMeanings } from '../../components/format'
import { auditDetail, auditText } from './audit'
import {
  Meter,
  Actions,
  Badge,
  Button,
  Card,
  Checkbox,
  ConfirmButton,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Facts,
  Field,
  Fieldset,
  Grow,
  List,
  ListRow,
  Loaded,
  LoadMore,
  Note,
  PageBody,
  Row,
  Select,
  Stack,
  Table,
  Tabs,
  TBody,
  Td,
  Text,
  TextArea,
  TextInput,
  Th,
  THead,
  Tr,
  useToast,
  VisuallyHidden,
} from '../../ui'

/** The tenant admin layout: a sub-navigation and one admin page. */
export function AdminLayout() {
  const { tenant, isAdmin } = useTenant()
  if (!isAdmin) {
    return (
      <PageBody>
        <h1>Space settings</h1>
        <p>Only admins and owners of {tenant.name} can open these pages.</p>
      </PageBody>
    )
  }
  const tabs: [string, string][] = [
    ['members', 'Members'],
    ['invitations', 'Invitations'],
    ['policy', 'Sharing rules'],
    ['limits', 'Plan'],
    ['audit', 'Activity log'],
  ]
  return (
    <PageBody>
      <h1>Space settings: {tenant.name}</h1>
      <Tabs label="Admin" items={tabs.map(([to, label]) => ({ to, label }))} />
      <Outlet />
    </PageBody>
  )
}

const assignable: Schemas['MemberRole'][] = ['guest', 'member', 'admin', 'owner']

export function MembersPage() {
  const { tenant, role: myRole } = useTenant()
  const { me } = useSession()
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const members = usePagedApi(
    (cursor) =>
      unwrap(
        client.GET('/v1/tenants/{tenant_id}/members', { params: { path: { tenant_id: tenant.id }, query: cursor ? { cursor } : {} } }),
      ),
    [tenant.id],
  )

  // A change to Guest takes away everything that is not shared with the person directly, so it
  // asks first and says so (the role select alone gave no hint).
  const [toGuest, setToGuest] = useState<Schemas['Member'] | null>(null)

  async function changeRole(m: Schemas['Member'], role: Schemas['MemberRole']) {
    setError(undefined)
    try {
      await unwrap(
        client.PATCH('/v1/tenants/{tenant_id}/members/{user_id}', {
          params: { path: { tenant_id: tenant.id, user_id: m.user_id } },
          body: { role },
        }),
      )
      toast(`${m.display_name} is now ${memberRoleLabels[role].toLowerCase()}.`)
    } catch (e) {
      setError(e)
    }
    members.reload()
  }

  return (
    <section aria-labelledby="members-title">
      <h2 id="members-title">Members</h2>
      <ErrorNote error={error} />
      {toGuest && (
        <ConfirmDialog
          title={`Make ${toGuest.display_name} a guest?`}
          message={
            <p>
              {toGuest.display_name} then sees only the items that are shared with them directly, and loses access to everything else in{' '}
              {tenant.name} at once. You can make them a {memberRoleLabels[toGuest.role].toLowerCase()} again later.
            </p>
          }
          confirmLabel="Make guest"
          onConfirm={() => changeRole(toGuest, 'guest')}
          onClose={() => setToGuest(null)}
        />
      )}
      <Loaded state={members}>
        {({ items }) => (
          <Table caption="Members">
            <THead>
              <Tr>
                <Th scope="col">Name</Th>
                <Th hideNarrow scope="col">
                  Email
                </Th>
                <Th scope="col">Role</Th>
                <Th scope="col">
                  <VisuallyHidden>Actions</VisuallyHidden>
                </Th>
              </Tr>
            </THead>
            <TBody>
              {items.map((m) => {
                const self = m.user_id === me.user.id
                // Only owners can make or change owners.
                const locked = self || (m.role === 'owner' && myRole !== 'owner')
                return (
                  <Tr key={m.user_id}>
                    <Th scope="row">
                      {m.display_name}
                      {self && (
                        <Text as="span" tone="muted">
                          {' '}
                          (you)
                        </Text>
                      )}
                    </Th>
                    <Td hideNarrow>{m.email}</Td>
                    <Td>
                      <Select
                        aria-label={`Role of ${m.display_name}`}
                        value={m.role}
                        disabled={locked}
                        onChange={(e) => {
                          const role = e.target.value as Schemas['MemberRole']
                          if (role === 'guest' && m.role !== 'guest') setToGuest(m)
                          else void changeRole(m, role)
                        }}
                      >
                        {assignable
                          .filter((r) => r !== 'owner' || myRole === 'owner' || m.role === 'owner')
                          .map((r) => (
                            <option key={r} value={r}>
                              {memberRoleLabels[r]}
                            </option>
                          ))}
                      </Select>
                    </Td>
                    <Td actions>
                      {!locked && (
                        <ConfirmButton
                          label="Remove"
                          ariaLabel={`Remove ${m.display_name}`}
                          title={`Remove ${m.display_name} from ${tenant.name}?`}
                          message="They lose access to everything in this space at once. Their agents are paused, their links are turned off, and their app passwords stop working. Their files stay."
                          confirmLabel={`Remove ${m.display_name}`}
                          danger
                          onConfirm={async () => {
                            await unwrap(
                              client.DELETE('/v1/tenants/{tenant_id}/members/{user_id}', {
                                params: { path: { tenant_id: tenant.id, user_id: m.user_id } },
                              }),
                            )
                            toast(`Removed ${m.display_name} from ${tenant.name}.`)
                            members.reload()
                          }}
                        />
                      )}
                    </Td>
                  </Tr>
                )
              })}
            </TBody>
          </Table>
        )}
      </Loaded>
      <LoadMore state={members} />
    </section>
  )
}

/** The member roles a space can still take, from its plan: a personal space has room only for its owner. */
function useRoomForMembers(tenantId: string) {
  return useApi(async () => {
    const res = await unwrap(client.GET('/v1/tenants/{tenant_id}/limits', { params: { path: { tenant_id: tenantId } } }))
    return res.limits.members === null || res.usage.members < res.limits.members
  }, [tenantId])
}

export function InvitationsPage() {
  const { tenant } = useTenant()
  const { toast } = useToast()
  const room = useRoomForMembers(tenant.id)
  // A personal space is for one person: it takes guests, not members (the plan's member limit).
  const membersAllowed = tenant.kind !== 'personal' && room.data !== false
  const [emails, setEmails] = useState('')
  const [role, setRole] = useState<Schemas['CreateInvitationRequest']['role']>(tenant.kind === 'personal' ? 'guest' : 'member')
  const [message, setMessage] = useState('')
  const [error, setError] = useState<unknown>()
  const [startTeam, setStartTeam] = useState(false)
  const invitations = usePagedApi(
    (cursor) =>
      unwrap(
        client.GET('/v1/tenants/{tenant_id}/invitations', { params: { path: { tenant_id: tenant.id }, query: cursor ? { cursor } : {} } }),
      ),
    [tenant.id],
  )
  const effectiveRole = membersAllowed ? role : 'guest'

  async function invite(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    const list = emails
      .split(/[\s,;]+/)
      .map((x) => x.trim())
      .filter(Boolean)
    const sent: string[] = []
    try {
      for (const email of list) {
        await unwrap(
          client.POST('/v1/tenants/{tenant_id}/invitations', {
            params: { path: { tenant_id: tenant.id } },
            body: { email, role: effectiveRole, ...(message.trim() ? { message: message.trim() } : {}) },
          }),
        )
        sent.push(email)
      }
    } catch (err) {
      setError(err)
    }
    if (sent.length) {
      toast(sent.length === 1 ? `Invitation sent to ${sent[0]}.` : `${sent.length} invitations sent.`)
      setEmails(list.filter((x) => !sent.includes(x)).join(', '))
      if (sent.length === list.length) setMessage('')
      invitations.reload()
    }
  }

  return (
    <section aria-labelledby="inv-title">
      <h2 id="inv-title">Invitations</h2>
      <Card as="form" onSubmit={invite} aria-label="Invite people">
        <Row align="end">
          <Field label="Emails" grow>
            <TextInput
              required
              value={emails}
              onChange={(e) => setEmails(e.target.value)}
              placeholder="name@example.com"
              aria-describedby="inv-emails-help"
            />
          </Field>
          <Field label="Role">
            <Select value={effectiveRole} onChange={(e) => setRole(e.target.value as typeof role)} aria-describedby="inv-role-help">
              <option value="guest">Guest</option>
              {membersAllowed && <option value="member">Member</option>}
              {membersAllowed && <option value="admin">Admin</option>}
            </Select>
          </Field>
        </Row>
        <Text tone="muted" size="sm" id="inv-emails-help">
          One or more, separated by commas
        </Text>
        <Text tone="muted" size="sm" id="inv-role-help">
          {memberRoleLabels[effectiveRole]}: {memberRoleMeanings[effectiveRole]}
        </Text>
        {!membersAllowed && (
          <Note>
            <p>
              {tenant.kind === 'personal'
                ? `${tenant.name} is a personal space, just for you. Invite people as guests to share items with them, or start a team space to work together.`
                : `${tenant.name} has reached the member limit of its plan. Invite people as guests, or change the plan.`}
            </p>
            {tenant.kind === 'personal' && <Button onClick={() => setStartTeam(true)}>Start a team space</Button>}
          </Note>
        )}
        <Field label="Message (optional)">
          <TextArea rows={2} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
        {!tenant.policy.allow_guests && effectiveRole === 'guest' && (
          <Text tone="danger">Guests are turned off in the sharing rules of this space.</Text>
        )}
        <ErrorNote error={error} context={{ space: tenant.name }} />
        <Actions>
          <Button variant="primary" type="submit">
            Send invitations
          </Button>
        </Actions>
      </Card>
      <Loaded state={invitations}>
        {({ items }) => (
          <List>
            {items.length === 0 && (
              <Text as="li" tone="muted">
                No open invitations.
              </Text>
            )}
            {items.map((i) => (
              <ListRow key={i.id}>
                <Grow>
                  {i.email} <Badge>{memberRoleLabels[i.role]}</Badge>
                  <Text as="span" tone="muted" size="sm" block>
                    Invited, waiting. Ends{' '}
                    {new Date(i.expires_at).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })}.
                  </Text>
                </Grow>
                <ConfirmButton
                  label="Cancel invitation"
                  ariaLabel={`Cancel invitation: ${i.email}`}
                  title="Cancel this invitation?"
                  message={`${i.email} can no longer join with it.`}
                  confirmLabel="Cancel invitation"
                  danger
                  onConfirm={async () => {
                    await unwrap(
                      client.DELETE('/v1/tenants/{tenant_id}/invitations/{invitation_id}', {
                        params: { path: { tenant_id: tenant.id, invitation_id: i.id } },
                      }),
                    )
                    invitations.reload()
                  }}
                />
              </ListRow>
            ))}
          </List>
        )}
      </Loaded>
      <LoadMore state={invitations} />
      {startTeam && <StartTeamDialog onClose={() => setStartTeam(false)} />}
    </section>
  )
}

export function PolicyPage() {
  const { tenant } = useTenant()
  const { reloadTenants } = useSession()
  const { toast } = useToast()
  const [name, setName] = useState(tenant.name)
  const [policy, setPolicy] = useState<Schemas['TenantPolicy']>(tenant.policy)
  const [error, setError] = useState<unknown>()

  async function save(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      await unwrap(
        client.PATCH('/v1/tenants/{tenant_id}', { params: { path: { tenant_id: tenant.id } }, body: { name: name.trim(), policy } }),
      )
      toast('Saved the sharing rules.')
      await reloadTenants()
    } catch (err) {
      setError(err)
    }
  }

  const check = (key: keyof Omit<Schemas['TenantPolicy'], 'share_link_max_days'>, label: string, disabled = false) => (
    <Checkbox
      checked={policy[key]}
      disabled={disabled}
      onChange={(e) => setPolicy({ ...policy, [key]: e.target.checked })}
      label={<>{label}</>}
    />
  )

  return (
    <section aria-labelledby="policy-title">
      <h2 id="policy-title">Sharing rules</h2>
      <Card as="form" onSubmit={save}>
        <Field label="Space name">
          <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Fieldset legend="Sharing">
          {check('members_can_share', 'Members can share')}
          {check('allow_guests', 'Allow guests (people outside this space)')}
          {check('allow_share_links', 'Allow links')}
          {check('allow_edit_links', 'Allow links that can edit', !policy.allow_share_links)}
          <Field label="Links end after at most (days, empty for no limit)">
            <TextInput
              type="number"
              min={1}
              value={policy.share_link_max_days ?? ''}
              disabled={!policy.allow_share_links}
              onChange={(e) => setPolicy({ ...policy, share_link_max_days: e.target.value ? Number(e.target.value) : null })}
            />
          </Field>
        </Fieldset>
        <ErrorNote error={error} />
        <Actions>
          <Button variant="primary" type="submit">
            Save
          </Button>
        </Actions>
      </Card>
    </section>
  )
}

function UsageMeter({
  label,
  used,
  limit,
  format = String,
}: {
  label: string
  used: number
  limit: number | null
  format?: (n: number) => string
}) {
  return (
    <Stack gap={1}>
      <Row>
        <Grow>{label}</Grow>
        <span>
          {format(used)} of {limit === null ? 'no limit' : format(limit)}
        </span>
      </Row>
      {limit !== null && <Meter max={limit} value={used} high={limit * 0.9} label={label} />}
    </Stack>
  )
}

export function LimitsPage() {
  const { tenant } = useTenant()
  const limits = useApi(
    () => unwrap(client.GET('/v1/tenants/{tenant_id}/limits', { params: { path: { tenant_id: tenant.id } } })),
    [tenant.id],
  )
  return (
    <section aria-labelledby="limits-title">
      <h2 id="limits-title">Plan</h2>
      <Loaded state={limits}>
        {({ plan, limits: l, usage: u }) => (
          <Card as="div">
            <p>
              Plan: <strong>{plan[0].toUpperCase() + plan.slice(1)}</strong>
            </p>
            <UsageMeter label="Storage" used={u.storage_bytes} limit={l.storage_bytes} format={formatBytes} />
            <UsageMeter label="Agents" used={u.agents} limit={l.agents} />
            <UsageMeter label="Members" used={u.members} limit={l.members} />
            <Facts>
              <dt>Largest file</dt>
              <dd>{formatBytes(l.max_file_bytes)}</dd>
              <dt>Versions kept</dt>
              <dd>
                {l.version_days === null ? 'Forever' : `${l.version_days} days`}
                {l.version_count !== null && `, at most ${l.version_count} per file`}. Named versions are kept forever.
              </dd>
              <dt>Trash kept</dt>
              <dd>{l.trash_days} days</dd>
              <dt>Admin restore</dt>
              <dd>{l.admin_restore_days ? `${l.admin_restore_days} days` : 'Not in this plan'}</dd>
              <dt>Items</dt>
              <dd>{u.nodes}</dd>
            </Facts>
          </Card>
        )}
      </Loaded>
    </section>
  )
}

export function AuditPage() {
  const { tenant } = useTenant()
  const [action, setAction] = useState('')
  const [filter, setFilter] = useState('')
  // The activity log also comes as NDJSON (for export); the UI asks for JSON pages.
  const audit = usePagedApi(
    async (cursor) =>
      (await unwrap(
        client.GET('/v1/tenants/{tenant_id}/audit', {
          params: { path: { tenant_id: tenant.id }, query: { ...(filter ? { action: filter } : {}), ...(cursor ? { cursor } : {}) } },
        }),
      )) as Schemas['AuditPage'],
    [tenant.id, filter],
  )
  // The items that entries name, by ID: their name and whether they are a folder. An item the
  // admin can't open any more (trashed for good) stays "an item".
  const nodeIds = [
    ...new Set((audit.data?.items ?? []).filter((a) => a.target_type === 'node' && a.target_id).map((a) => a.target_id as string)),
  ]
  const items_ = useApi(async () => {
    const out: Record<string, AuditItem> = {}
    await Promise.all(
      nodeIds.map(async (id) => {
        try {
          const n = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: id } } }))
          out[id] = { name: n.parent_id ? n.name : 'Files', folder: n.kind === 'folder' }
        } catch {
          // Not readable: the row says "an item".
        }
      }),
    )
    return out
  }, [nodeIds.join(',')])
  return (
    <section aria-labelledby="audit-title">
      <h2 id="audit-title">Activity log</h2>
      <Row
        as="form"
        align="end"
        onSubmit={(e) => {
          e.preventDefault()
          setFilter(action)
        }}
      >
        <Field label="Show" grow>
          <Select value={action} onChange={(e) => setAction(e.target.value)}>
            <option value="">Everything</option>
            <option value="auth.">Sign-ins</option>
            <option value="member.">Members</option>
            <option value="invitation.">Invitations</option>
            <option value="grant.">Access</option>
            <option value="share_link.">Links</option>
            <option value="agent.">Agents</option>
            <option value="node.">Items</option>
            <option value="version.">Versions</option>
          </Select>
        </Field>
        <Button type="submit">Show</Button>
      </Row>
      <Loaded state={audit}>
        {({ items }) => (
          <>
            {items.length === 0 ? (
              <EmptyState title="No activity yet" text={`Changes in ${tenant.name} show up here.`} />
            ) : (
              <List>
                {items.map((a) => (
                  <ListRow key={a.id}>
                    <Grow>
                      <Actor pair={a} /> <AuditWhat entry={a} items={items_.data ?? {}} />
                    </Grow>
                    <Text as="span" tone="muted" size="sm" title={formatDate(a.at)}>
                      {formatWhen(a.at)}
                    </Text>
                  </ListRow>
                ))}
              </List>
            )}
          </>
        )}
      </Loaded>
      <LoadMore state={audit} />
    </section>
  )
}

type AuditItem = { name: string; folder: boolean }

/** What an activity log entry did, with the item, the person and the old and new values where the entry has them. */
function AuditWhat({ entry, items }: { entry: Schemas['AuditEntry']; items: Record<string, AuditItem> }) {
  const { tenant } = useTenant()
  const names = useNames()
  const parts = auditDetail(entry, { person: (id) => names.person(id)?.name, spaceName: tenant.name })
  if (!parts) return <>{auditText(entry)}</>
  return (
    <>
      {parts.map((p, i) => {
        if (typeof p === 'string') return <span key={i}>{p}</span>
        const it = items[p.item]
        if (!it) return <span key={i}>an item</span>
        return (
          <Link key={i} to={it.folder ? tenantPath(tenant.id, 'files', p.item) : tenantPath(tenant.id, 'item', p.item)}>
            {it.name}
          </Link>
        )
      })}
    </>
  )
}
