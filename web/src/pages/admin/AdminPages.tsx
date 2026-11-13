import { Fragment, useState, type FormEvent } from 'react'
import { Link, Outlet, useLocation } from 'react-router'
import { client, RuumApiError, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { Actor } from '../../app/names'
import { useNames } from '../../app/useNames'
import { StartTeamDialog } from '../../app/Shell'
import {
  formatBytes,
  formatDate,
  formatDay,
  formatNumber,
  formatWhen,
  memberRoleInline,
  memberRoleLabels,
  memberRoleMeanings,
  roleInline,
  roleLabels,
} from '../../components/format'
import { auditClient, auditDetail, auditText } from './audit'
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
  Indent,
  List,
  ListRow,
  Loaded,
  LoadMore,
  Menu,
  NarrowMeta,
  Note,
  PageBody,
  Radio,
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
  type MenuItem,
} from '../../ui'
import { t, tr, type MessageKey } from '../../i18n'

/**
 * The tenant admin layout: a sub-navigation and one admin page. Members (not guests) who are not
 * admins see only the people of the space, read-only (members may list members).
 */
export function AdminLayout() {
  const { tenant, isAdmin, role } = useTenant()
  const location = useLocation()
  const membersPage = location.pathname.endsWith('/admin/members')
  if (!isAdmin && (role !== 'member' || !membersPage)) {
    return (
      <PageBody>
        <h1>{t('shell.nav.admin')}</h1>
        <p>{t('admin.only', { space: tenant.name })}</p>
      </PageBody>
    )
  }
  const tabs: [string, MessageKey][] = isAdmin
    ? [
        ['members', 'admin.members'],
        ['invitations', 'admin.invitations'],
        ['policy', 'admin.policy'],
        ['limits', 'admin.plan'],
        ['audit', 'admin.audit'],
      ]
    : [['members', 'admin.members']]
  return (
    <PageBody>
      <h1>{t('admin.title', { space: tenant.name })}</h1>
      {isAdmin ? <Tabs label={t('admin.tabs')} items={tabs.map(([to, label]) => ({ to, label: t(label) }))} /> : <Outlet />}
      {isAdmin && <Outlet />}
    </PageBody>
  )
}

/** The member roles in the order of the role menu, with Owner last. */
const assignable: Schemas['MemberRole'][] = ['guest', 'member', 'admin', 'owner']
const contentRoles: Schemas['ContentRole'][] = ['view', 'comment', 'edit', 'manage']

/** The help line of a content role (ui-followups item 1). */
export function contentRoleHelp(role: Schemas['ContentRole'], space: string): string {
  return t('contentRole.help', { role, space })
}

export function MembersPage() {
  const { tenant, role: myRole, isAdmin } = useTenant()
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
  // A change to Member asks for the content role in the same step (default Can manage).
  const [toMember, setToMember] = useState<Schemas['Member'] | null>(null)
  const [removing, setRemoving] = useState<Schemas['Member'] | null>(null)

  async function update(m: Schemas['Member'], body: { role: Schemas['MemberRole']; content_role?: Schemas['ContentRole'] }, done: string) {
    setError(undefined)
    try {
      await unwrap(
        client.PATCH('/v1/tenants/{tenant_id}/members/{user_id}', {
          params: { path: { tenant_id: tenant.id, user_id: m.user_id } },
          body,
        }),
      )
      toast(done)
    } catch (e) {
      setError(e)
    }
    members.reload()
  }

  const changeRole = (m: Schemas['Member'], role: Schemas['MemberRole']) =>
    update(m, { role }, t('members.changed', { name: m.display_name, role: memberRoleInline[role] }))

  const changeContent = (m: Schemas['Member'], content_role: Schemas['ContentRole']) =>
    update(m, { role: 'member', content_role }, t('members.content.changed', { name: m.display_name, role: roleInline[content_role], space: tenant.name }))

  return (
    <section aria-labelledby="members-title">
      <h2 id="members-title">{t('admin.members')}</h2>
      {!isAdmin && <Text tone="muted">{t('members.readOnly', { space: tenant.name })}</Text>}
      <ErrorNote error={error} />
      {toGuest && (
        <ConfirmDialog
          title={t('members.toGuest.title', { name: toGuest.display_name })}
          message={
            <p>
              {t('members.toGuest.message', {
                name: toGuest.display_name,
                space: tenant.name,
                role: memberRoleInline[toGuest.role],
              })}
            </p>
          }
          confirmLabel={t('members.toGuest.confirm')}
          onConfirm={() => changeRole(toGuest, 'guest')}
          onClose={() => setToGuest(null)}
        />
      )}
      {toMember && (
        <MakeMemberDialog
          member={toMember}
          space={tenant.name}
          onClose={() => setToMember(null)}
          onConfirm={(content_role) =>
            update(toMember, { role: 'member', content_role }, t('members.changed', { name: toMember.display_name, role: memberRoleInline.member }))
          }
        />
      )}
      {removing && (
        <ConfirmDialog
          title={t('members.remove.title', { name: removing.display_name, space: tenant.name })}
          message={t('members.remove.message')}
          confirmLabel={t('members.remove.label', { name: removing.display_name })}
          danger
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            await unwrap(
              client.DELETE('/v1/tenants/{tenant_id}/members/{user_id}', {
                params: { path: { tenant_id: tenant.id, user_id: removing.user_id } },
              }),
            )
            toast(t('members.removed', { name: removing.display_name, space: tenant.name }))
            members.reload()
          }}
        />
      )}
      <Loaded state={members} skeleton={4} label={t('admin.members')}>
        {({ items }) => {
          const owners = items.filter((m) => m.role === 'owner').length
          return (
            <Table caption={t('admin.members')}>
              <THead>
                <Tr>
                  <Th scope="col">{t('common.name')}</Th>
                  <Th hideNarrow scope="col">
                    {t('account.email')}
                  </Th>
                  <Th scope="col">{t('members.role')}</Th>
                  {isAdmin && (
                    <Th scope="col">
                      <VisuallyHidden>{t('common.actions')}</VisuallyHidden>
                    </Th>
                  )}
                </Tr>
              </THead>
              <TBody>
                {items.map((m) => {
                  const self = m.user_id === me.user.id
                  // Why a role can't change here, said next to it (journeys J6).
                  const why: string | null = !isAdmin
                    ? null
                    : self
                      ? m.role === 'owner' && owners === 1
                        ? t('members.locked.lastOwner')
                        : t('members.locked.self')
                      : m.role === 'owner' && myRole !== 'owner'
                        ? t('members.locked.owner')
                        : null
                  const locked = !isAdmin || why !== null
                  const hintId = `member-${m.user_id}-why`
                  const roleItems: MenuItem[] = assignable
                    .filter((r) => r !== 'owner' || myRole === 'owner' || m.role === 'owner')
                    .map((r) => ({
                      label: memberRoleLabels[r],
                      hint: memberRoleMeanings[r],
                      checked: m.role === r,
                      onSelect: () => {
                        if (r === m.role) return
                        if (r === 'guest') setToGuest(m)
                        else if (r === 'member') setToMember(m)
                        else void changeRole(m, r)
                      },
                    }))
                  const content = m.role === 'member' ? (m.content_role ?? 'manage') : null
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
                        <NarrowMeta>{m.email}</NarrowMeta>
                      </Th>
                      <Td hideNarrow>{m.email}</Td>
                      <Td>
                        <Stack gap={1}>
                          <Row gap={2}>
                            {locked ? (
                              <Text as="span" aria-describedby={why ? hintId : undefined}>
                                {memberRoleLabels[m.role]}
                              </Text>
                            ) : (
                              <Menu label={memberRoleLabels[m.role]} context={t('members.role.of', { name: m.display_name })} items={roleItems} size="sm" />
                            )}
                            {content &&
                              (isAdmin ? (
                                <Menu
                                  label={roleLabels[content]}
                                  context={t('members.content.of', { name: m.display_name })}
                                  size="sm"
                                  items={contentRoles.map((r) => ({
                                    label: roleLabels[r],
                                    hint: contentRoleHelp(r, tenant.name),
                                    checked: content === r,
                                    onSelect: () => {
                                      if (r !== content) void changeContent(m, r)
                                    },
                                  }))}
                                />
                              ) : (
                                <Text as="span" tone="muted">
                                  {roleLabels[content]}
                                </Text>
                              ))}
                          </Row>
                          {why && (
                            <Text as="span" tone="muted" size="sm" id={hintId}>
                              {why}
                            </Text>
                          )}
                          {content && (content === 'view' || content === 'comment') && (
                            <Text as="span" tone="muted" size="sm">
                              {t('members.content.everyone')}
                            </Text>
                          )}
                        </Stack>
                      </Td>
                      {isAdmin && (
                        <Td actions>
                          {!locked && (
                            <Menu
                              icon="more"
                              ariaLabel={t('members.more', { name: m.display_name })}
                              quiet
                              align="end"
                              items={[{ label: t('members.remove.item', { space: tenant.name }), danger: true, onSelect: () => setRemoving(m) }]}
                            />
                          )}
                        </Td>
                      )}
                    </Tr>
                  )
                })}
              </TBody>
            </Table>
          )
        }}
      </Loaded>
      <LoadMore state={members} />
    </section>
  )
}

/** "Make *name* a member?": the content role in the same step (default Can manage). */
function MakeMemberDialog(props: {
  member: Schemas['Member']
  space: string
  onClose: () => void
  onConfirm: (content: Schemas['ContentRole']) => Promise<void>
}) {
  const [content, setContent] = useState<Schemas['ContentRole']>('manage')
  return (
    <ConfirmDialog
      title={t('members.toMember.title', { name: props.member.display_name })}
      message={
        <Stack>
          <p>{t('members.toMember.message', { name: props.member.display_name, space: props.space })}</p>
          <Fieldset legend={t('members.content.label')} plain>
            {contentRoles.map((r) => (
              <Stack key={r} gap={1}>
                <Radio name="content-role" checked={content === r} onChange={() => setContent(r)} label={roleLabels[r]} />
                <Indent>
                  <Text tone="muted" size="sm">
                    {contentRoleHelp(r, props.space)}
                  </Text>
                </Indent>
              </Stack>
            ))}
          </Fieldset>
        </Stack>
      }
      confirmLabel={t('members.toMember.confirm')}
      onConfirm={() => props.onConfirm(content)}
      onClose={props.onClose}
    />
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
  const [content, setContent] = useState<Schemas['ContentRole']>('manage')
  const [message, setMessage] = useState('')
  const [error, setError] = useState<unknown>()
  // The address the server refused as not an email address: the note names it (a field error
  // the generic copy cannot point at).
  const [badEmail, setBadEmail] = useState<string | null>(null)
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
    setBadEmail(null)
    const list = emails
      .split(/[\s,;]+/)
      .map((x) => x.trim())
      .filter(Boolean)
    const sent: string[] = []
    let current = ''
    try {
      for (const email of list) {
        current = email
        await unwrap(
          client.POST('/v1/tenants/{tenant_id}/invitations', {
            params: { path: { tenant_id: tenant.id } },
            body: {
              email,
              role: effectiveRole,
              // Only a member has a content role; for a guest or an admin it is left out.
              ...(effectiveRole === 'member' ? { content_role: content } : {}),
              ...(message.trim() ? { message: message.trim() } : {}),
            },
          }),
        )
        sent.push(email)
      }
    } catch (err) {
      if (err instanceof RuumApiError && err.code === 'validation_failed') setBadEmail(current)
      else setError(err)
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
            <Select width="auto" value={effectiveRole} onChange={(e) => setRole(e.target.value as typeof role)} aria-describedby="inv-role-help">
              <option value="guest">{memberRoleLabels.guest}</option>
              {membersAllowed && <option value="member">{memberRoleLabels.member}</option>}
              {membersAllowed && <option value="admin">{memberRoleLabels.admin}</option>}
            </Select>
          </Field>
          {effectiveRole === 'member' && (
            <Field label={t('members.content.label')}>
              <Select width="auto" value={content} onChange={(e) => setContent(e.target.value as Schemas['ContentRole'])} aria-describedby="inv-content-help">
                {contentRoles.map((r) => (
                  <option key={r} value={r}>
                    {roleLabels[r]}
                  </option>
                ))}
              </Select>
            </Field>
          )}
        </Row>
        <Text tone="muted" size="sm" id="inv-emails-help">
          {t('invitations.emails.help')}
        </Text>
        <Text tone="muted" size="sm" id="inv-role-help">
          {t('share.roleMeaning', { role: memberRoleLabels[effectiveRole], meaning: memberRoleMeanings[effectiveRole] })}
        </Text>
        {effectiveRole === 'member' && (
          <Text tone="muted" size="sm" id="inv-content-help">
            {t('share.roleMeaning', { role: roleLabels[content], meaning: contentRoleHelp(content, tenant.name) })}
          </Text>
        )}
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
        {badEmail && (
          <Note tone="danger" role="alert">
            {t('invitations.badEmail', { email: badEmail })}
          </Note>
        )}
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
                  {i.role === 'member' && i.content_role && <Badge>{roleLabels[i.content_role]}</Badge>}
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
      {tenant.kind === 'personal' && <AppAddressCard />}
    </section>
  )
}

/** The rules of a tenant slug (docs/STEP3.md, "Host names and slugs"), checked before the server does. */
const SLUG_PATTERN = /^(?!xn-)[a-z0-9](?:[a-z0-9]|-(?!-)){1,38}[a-z0-9]$/

/**
 * The address of a personal space's apps (card S-35, decision 0008-1). Before the first app the
 * space has none: it will get a random one (`ws-7k2m9q`), or the person chooses a readable one
 * now. The choice is public (DNS, certificate logs) and permanent, so a dialog asks first.
 */
function AppAddressCard() {
  const { tenant } = useTenant()
  const { toast } = useToast()
  const current = useApi(() => unwrap(client.GET('/v1/tenants/{tenant_id}', { params: { path: { tenant_id: tenant.id } } })), [tenant.id])
  const [slug, setSlug] = useState('')
  const [asking, setAsking] = useState(false)
  const [problem, setProblem] = useState<string | null>(null)
  const wanted = slug.trim()

  function ask(e: FormEvent) {
    e.preventDefault()
    setProblem(SLUG_PATTERN.test(wanted) ? null : t('appAddress.invalid'))
    if (SLUG_PATTERN.test(wanted)) setAsking(true)
  }

  // A refusal that the person can fix closes the dialog and shows under the field (copy.md).
  async function choose() {
    try {
      await unwrap(client.PATCH('/v1/tenants/{tenant_id}', { params: { path: { tenant_id: tenant.id } }, body: { app_slug: wanted } }))
    } catch (err) {
      if (!(err instanceof RuumApiError)) throw err
      if (err.hint?.includes('app_slug_taken')) return setProblem(t('appAddress.taken', { slug: wanted }))
      if (err.hint?.includes('app_slug_fixed')) {
        toast(t('appAddress.fixedNow'))
        return current.reload()
      }
      if (err.code === 'validation_failed') return setProblem(t('appAddress.invalid'))
      throw err
    }
    toast(t('appAddress.saved', { slug: wanted }))
    setSlug('')
    current.reload()
  }

  return (
    <Card aria-labelledby="app-address-title">
      <h3 id="app-address-title">{t('appAddress.title')}</h3>
      <Loaded state={current}>
        {(tn) =>
          tn.app_slug ? (
            <Text>{tr('appAddress.fixed', { slug: tn.app_slug, b: (chunks) => <strong>{chunks}</strong> })}</Text>
          ) : (
            <Stack as="form" gap={4} onSubmit={ask}>
              <Text>{t('appAddress.random')}</Text>
              <Field label={t('appAddress.label')} hint={t('appAddress.hint')} error={problem} hintId="app-address-hint" errorId="app-address-error">
                <TextInput
                  value={slug}
                  maxLength={40}
                  autoComplete="off"
                  spellCheck={false}
                  aria-describedby={problem ? 'app-address-hint app-address-error' : 'app-address-hint'}
                  aria-invalid={problem ? true : undefined}
                  onChange={(e) => (setSlug(e.target.value.toLowerCase()), setProblem(null))} // i18n-ignore: a slug is lower case by rule
                />
              </Field>
              <Actions>
                <Button type="submit" disabled={!wanted}>
                  {t('appAddress.submit')}
                </Button>
              </Actions>
            </Stack>
          )
        }
      </Loaded>
      {asking && (
        <ConfirmDialog
          title={t('appAddress.confirm.title', { slug: wanted })}
          message={<p>{t('appAddress.confirm.message')}</p>}
          confirmLabel={t('appAddress.confirm.button', { slug: wanted })}
          onClose={() => setAsking(false)}
          onConfirm={choose}
        />
      )}
    </Card>
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
      <Field label={t('audit.show')} hint={filter === 'auth.' ? t('audit.signIns.help') : undefined} hintId="audit-filter-help">
        <Select width="auto" value={filter} onChange={(e) => setFilter(e.target.value)} aria-describedby={filter === 'auth.' ? 'audit-filter-help' : undefined}>
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
      <Loaded state={audit} skeleton={6} label={t('admin.audit')}>
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
                      {a.action === 'agent.key_refused' && (
                        <>
                          {' '}
                          <Badge tone="warning">{t('audit.refused')}</Badge>
                        </>
                      )}
                      {auditClient(a) && (
                        <Text as="span" tone="muted" size="sm" block>
                          {auditClient(a)}
                        </Text>
                      )}
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
  const { data: passwords } = useApi(
    () => (entry.action === 'auth.app_password_used' ? unwrap(client.GET('/v1/me/app-passwords')) : Promise.resolve(null)),
    [entry.action],
  )
  const detail = auditDetail(entry, {
    person: (id) => names.person(id)?.name,
    spaceName: tenant.name,
    appPassword: (id) => passwords?.items.find((p) => p.id === id)?.name,
  })
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
