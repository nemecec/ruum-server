import { Fragment, useState, type FormEvent } from 'react'
import { Link, Outlet } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { Actor } from '../../app/names'
import { useNames } from '../../app/useNames'
import { StartTeamDialog } from '../../app/Shell'
import { formatBytes, formatDate, formatDay, formatNumber, formatWhen, memberRoleLabels, memberRoleMeanings } from '../../components/format'
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
import { t, tr, type MessageKey } from '../../i18n'

/** The tenant admin layout: a sub-navigation and one admin page. */
export function AdminLayout() {
  const { tenant, isAdmin } = useTenant()
  if (!isAdmin) {
    return (
      <PageBody>
        <h1>{t('shell.nav.admin')}</h1>
        <p>{t('admin.only', { space: tenant.name })}</p>
      </PageBody>
    )
  }
  const tabs: [string, MessageKey][] = [
    ['members', 'admin.members'],
    ['invitations', 'admin.invitations'],
    ['policy', 'admin.policy'],
    ['limits', 'admin.plan'],
    ['audit', 'admin.audit'],
  ]
  return (
    <PageBody>
      <h1>{t('admin.title', { space: tenant.name })}</h1>
      <Tabs label={t('admin.tabs')} items={tabs.map(([to, label]) => ({ to, label: t(label) }))} />
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
      toast(t('members.changed', { name: m.display_name, role: memberRoleLabels[role].toLowerCase() }))
    } catch (e) {
      setError(e)
    }
    members.reload()
  }

  return (
    <section aria-labelledby="members-title">
      <h2 id="members-title">{t('admin.members')}</h2>
      <ErrorNote error={error} />
      {toGuest && (
        <ConfirmDialog
          title={t('members.toGuest.title', { name: toGuest.display_name })}
          message={
            <p>
              {t('members.toGuest.message', {
                name: toGuest.display_name,
                space: tenant.name,
                role: memberRoleLabels[toGuest.role].toLowerCase(),
              })}
            </p>
          }
          confirmLabel={t('members.toGuest.confirm')}
          onConfirm={() => changeRole(toGuest, 'guest')}
          onClose={() => setToGuest(null)}
        />
      )}
      <Loaded state={members}>
        {({ items }) => (
          <Table caption={t('admin.members')}>
            <THead>
              <Tr>
                <Th scope="col">{t('common.name')}</Th>
                <Th hideNarrow scope="col">
                  {t('account.email')}
                </Th>
                <Th scope="col">{t('members.role')}</Th>
                <Th scope="col">
                  <VisuallyHidden>{t('common.actions')}</VisuallyHidden>
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
                          {t('share.you')}
                        </Text>
                      )}
                    </Th>
                    <Td hideNarrow>{m.email}</Td>
                    <Td>
                      <Select
                        aria-label={t('members.role.of', { name: m.display_name })}
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
                          label={t('share.remove')}
                          ariaLabel={t('members.remove.label', { name: m.display_name })}
                          title={t('members.remove.title', { name: m.display_name, space: tenant.name })}
                          message={t('members.remove.message')}
                          confirmLabel={t('members.remove.label', { name: m.display_name })}
                          danger
                          onConfirm={async () => {
                            await unwrap(
                              client.DELETE('/v1/tenants/{tenant_id}/members/{user_id}', {
                                params: { path: { tenant_id: tenant.id, user_id: m.user_id } },
                              }),
                            )
                            toast(t('members.removed', { name: m.display_name, space: tenant.name }))
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
      toast(sent.length === 1 ? t('invitations.sent.one', { email: sent[0] }) : t('invitations.sent', { n: sent.length }))
      setEmails(list.filter((x) => !sent.includes(x)).join(', '))
      if (sent.length === list.length) setMessage('')
      invitations.reload()
    }
  }

  return (
    <section aria-labelledby="inv-title">
      <h2 id="inv-title">{t('admin.invitations')}</h2>
      <Card as="form" onSubmit={invite} aria-label={t('invitations.form')}>
        <Row align="end">
          <Field label={t('invitations.emails')} grow>
            <TextInput
              required
              value={emails}
              onChange={(e) => setEmails(e.target.value)}
              placeholder={t('share.email.placeholder')}
              aria-describedby="inv-emails-help"
            />
          </Field>
          <Field label={t('members.role')}>
            <Select value={effectiveRole} onChange={(e) => setRole(e.target.value as typeof role)} aria-describedby="inv-role-help">
              <option value="guest">{memberRoleLabels.guest}</option>
              {membersAllowed && <option value="member">{memberRoleLabels.member}</option>}
              {membersAllowed && <option value="admin">{memberRoleLabels.admin}</option>}
            </Select>
          </Field>
        </Row>
        <Text tone="muted" size="sm" id="inv-emails-help">
          {t('invitations.emails.help')}
        </Text>
        <Text tone="muted" size="sm" id="inv-role-help">
          {t('share.roleMeaning', { role: memberRoleLabels[effectiveRole], meaning: memberRoleMeanings[effectiveRole] })}
        </Text>
        {!membersAllowed && (
          <Note>
            <p>
              {tenant.kind === 'personal'
                ? t('invitations.personal', { space: tenant.name })
                : t('invitations.full', { space: tenant.name })}
            </p>
            {tenant.kind === 'personal' && <Button onClick={() => setStartTeam(true)}>{t('shell.startTeam.title')}</Button>}
          </Note>
        )}
        <Field label={t('invitations.message')}>
          <TextArea rows={2} value={message} onChange={(e) => setMessage(e.target.value)} />
        </Field>
        {!tenant.policy.allow_guests && effectiveRole === 'guest' && (
          <Text tone="danger">{t('invitations.guestsOff')}</Text>
        )}
        <ErrorNote error={error} context={{ space: tenant.name }} />
        <Actions>
          <Button variant="primary" type="submit">
            {t('invitations.send')}
          </Button>
        </Actions>
      </Card>
      <Loaded state={invitations}>
        {({ items }) => (
          <List>
            {items.length === 0 && (
              <Text as="li" tone="muted">
                {t('invitations.none')}
              </Text>
            )}
            {items.map((i) => (
              <ListRow key={i.id}>
                <Grow>
                  {i.email} <Badge>{memberRoleLabels[i.role]}</Badge>
                  <Text as="span" tone="muted" size="sm" block>
                    {t('invitations.waiting', { date: formatDay(i.expires_at) })}
                  </Text>
                </Grow>
                <ConfirmButton
                  label={t('invitations.cancel')}
                  ariaLabel={t('invitations.cancel.label', { email: i.email })}
                  title={t('invitations.cancel.title')}
                  message={t('invitations.cancel.message', { email: i.email })}
                  confirmLabel={t('invitations.cancel')}
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
      toast(t('policy.saved'))
      await reloadTenants()
    } catch (err) {
      setError(err)
    }
  }

  const check = (key: keyof Omit<Schemas['TenantPolicy'], 'share_link_max_days'>, label: MessageKey, disabled = false) => (
    <Checkbox
      checked={policy[key]}
      disabled={disabled}
      onChange={(e) => setPolicy({ ...policy, [key]: e.target.checked })}
      label={<>{t(label)}</>}
    />
  )

  return (
    <section aria-labelledby="policy-title">
      <h2 id="policy-title">{t('admin.policy')}</h2>
      <Card as="form" onSubmit={save}>
        <Field label={t('policy.name')}>
          <TextInput required value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Fieldset legend={t('policy.sharing')}>
          {check('members_can_share', 'policy.membersCanShare')}
          {check('allow_guests', 'policy.allowGuests')}
          {check('allow_share_links', 'policy.allowLinks')}
          {check('allow_edit_links', 'policy.allowEditLinks', !policy.allow_share_links)}
          <Field label={t('policy.linkDays')}>
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
            {t('common.save')}
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
  format = formatNumber,
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
          {limit === null ? t('limits.usage.unlimited', { used: format(used) }) : t('limits.usage', { used: format(used), limit: format(limit) })}
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
      <h2 id="limits-title">{t('admin.plan')}</h2>
      <Loaded state={limits}>
        {({ plan, limits: l, usage: u }) => (
          <Card as="div">
            <p>
              {tr('limits.plan', { plan: t('limits.plan.name', { plan }), b: (chunks) => <strong>{chunks}</strong> })}
            </p>
            <UsageMeter label={t('limits.storage')} used={u.storage_bytes} limit={l.storage_bytes} format={formatBytes} />
            <UsageMeter label={t('shell.nav.agents')} used={u.agents} limit={l.agents} />
            <UsageMeter label={t('admin.members')} used={u.members} limit={l.members} />
            <Facts>
              <dt>{t('limits.largestFile')}</dt>
              <dd>{formatBytes(l.max_file_bytes)}</dd>
              <dt>{t('limits.versions')}</dt>
              <dd>
                {t('limits.versions.value', {
                  forever: l.version_days === null ? 'yes' : 'no',
                  days: l.version_days ?? 0,
                  capped: l.version_count !== null ? 'yes' : 'no',
                  count: l.version_count ?? 0,
                })}
              </dd>
              <dt>{t('limits.trash')}</dt>
              <dd>{t('limits.days', { n: l.trash_days })}</dd>
              <dt>{t('limits.adminRestore')}</dt>
              <dd>{l.admin_restore_days ? t('limits.days', { n: l.admin_restore_days }) : t('limits.notInPlan')}</dd>
              <dt>{t('limits.items')}</dt>
              <dd>{formatNumber(u.nodes)}</dd>
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
          out[id] = { name: n.parent_id ? n.name : t('shell.nav.files'), folder: n.kind === 'folder' }
        } catch {
          // Not readable: the row says "an item".
        }
      }),
    )
    return out
  }, [nodeIds.join(',')])
  return (
    <section aria-labelledby="audit-title">
      <h2 id="audit-title">{t('admin.audit')}</h2>
      <Row
        as="form"
        align="end"
        onSubmit={(e) => {
          e.preventDefault()
          setFilter(action)
        }}
      >
        <Field label={t('audit.show')} grow>
          <Select value={action} onChange={(e) => setAction(e.target.value)}>
            <option value="">{t('audit.filter.all')}</option>
            <option value="auth.">{t('audit.filter.auth')}</option>
            <option value="member.">{t('admin.members')}</option>
            <option value="invitation.">{t('admin.invitations')}</option>
            <option value="grant.">{t('share.access')}</option>
            <option value="share_link.">{t('audit.filter.links')}</option>
            <option value="agent.">{t('shell.nav.agents')}</option>
            <option value="node.">{t('limits.items')}</option>
            <option value="version.">{t('details.versions')}</option>
          </Select>
        </Field>
        <Button type="submit">{t('audit.show')}</Button>
      </Row>
      <Loaded state={audit}>
        {({ items }) => (
          <>
            {items.length === 0 ? (
              <EmptyState title={t('audit.none.title')} text={t('audit.none.text', { space: tenant.name })} />
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
  const detail = auditDetail(entry, { person: (id) => names.person(id)?.name, spaceName: tenant.name })
  if (!detail) return <>{auditText(entry)}</>
  const it = detail.item ? items[detail.item] : undefined
  const item = !detail.item ? (
    ''
  ) : it ? (
    <Link to={it.folder ? tenantPath(tenant.id, 'files', detail.item) : tenantPath(tenant.id, 'item', detail.item)}>{it.name}</Link>
  ) : (
    <span>{t('audit.anItem')}</span>
  )
  // The words are spans of their own, beside the link (the tests and screen readers find them).
  const out = tr(detail.key, { ...detail.values, item })
  const parts = Array.isArray(out) ? out : [out]
  return <>{parts.map((p, i) => (typeof p === 'string' ? <span key={i}>{p}</span> : <Fragment key={i}>{p}</Fragment>))}</>
}
