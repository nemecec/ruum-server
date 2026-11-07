import { useEffect, useState, type FormEvent } from 'react'
import { Link } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useActorText, useNames, type Names } from '../../app/useNames'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDay, memberRoleInline, roleInline, roleLabels, roleMeanings } from '../../components/format'
import type { Node } from './api'
import {
  Button,
  Checkbox,
  ConfirmButton,
  Dialog,
  ErrorNote,
  Field,
  Grow,
  List,
  ListRow,
  Loaded,
  LoadMore,
  Menu,
  Note,
  Notice,
  Row,
  SecretBox,
  Select,
  Stack,
  Text,
  TextArea,
  TextInput,
} from '../../ui'
import { t, tr } from '../../i18n'

const roles: Schemas['Role'][] = ['view', 'comment', 'edit', 'manage']
const linkRoles: Schemas['ShareLinkRole'][] = ['view', 'comment', 'edit']

/** The roles a caller with `mine` may grant: at most its own role (decision 0001-1). */
function grantableRoles(mine: Schemas['Role'] | null | undefined): Schemas['Role'][] {
  const top = mine ? roles.indexOf(mine) : -1
  return roles.filter((_, i) => i <= top)
}

// The emails of email grants that this browser added. After the person signs in, the grant
// becomes a grant to their user, and `listGrants` gives no name or email for it (contract need
// C-7); the email the owner typed is then the best name the UI has. A per-viewer convenience.
const GRANT_EMAILS = 'ruum.grantEmails'

function grantEmails(): Record<string, string> {
  try {
    return JSON.parse(localStorage.getItem(GRANT_EMAILS) ?? '{}') as Record<string, string>
  } catch {
    return {}
  }
}

function rememberGrantEmail(grantId: string, email: string) {
  try {
    localStorage.setItem(GRANT_EMAILS, JSON.stringify({ ...grantEmails(), [grantId]: email }))
  } catch {
    // Storage can be off; the row then says "A person outside this space".
  }
}

/** The name of a grantee, never an ID. */
function granteeName(g: Schemas['Grantee'], names: Names, spaceName: string, grantId?: string): string {
  switch (g.type) {
    case 'user': {
      const person = names.person(g.id)
      return g.display_name ?? person?.name ?? g.email ?? (grantId ? grantEmails()[grantId] : undefined) ?? t('share.outsider')
    }
    case 'email':
      return g.email
    case 'tenant_members':
      return t('share.everyone', { space: spaceName })
  }
}

/** The second line of a grantee: the email under a name, or "Invited, not joined yet". */
function granteeDetail(g: Schemas['Grantee'], names: Names, grantId: string): string | null {
  if (g.type === 'email') return t('share.invitedNotJoined')
  if (g.type === 'user') {
    const email = g.email ?? names.person(g.id)?.email ?? grantEmails()[grantId]
    const name = g.display_name ?? names.person(g.id)?.name
    return name && email ? email : null
  }
  return null
}

/** The people of a space (members and guests), by user ID and by email. */
type SpacePeople = { byId: Map<string, Schemas['Member']>; byEmail: Map<string, Schemas['Member']> }

async function loadSpacePeople(tenantId: string): Promise<SpacePeople> {
  const byId = new Map<string, Schemas['Member']>()
  const byEmail = new Map<string, Schemas['Member']>()
  let cursor: string | undefined
  for (let i = 0; i < 20; i++) {
    const page = await unwrap(
      client.GET('/v1/tenants/{tenant_id}/members', { params: { path: { tenant_id: tenantId }, query: cursor ? { cursor } : {} } }),
    )
    for (const m of page.items) {
      byId.set(m.user_id, m)
      if (m.email) byEmail.set(m.email.toLowerCase(), m) // i18n-ignore: an email address as a key
    }
    if (!page.next_cursor) break
    cursor = page.next_cursor
  }
  return { byId, byEmail }
}

/**
 * Who an email is, for the add form: the caller, a person of the space (a share to them goes to
 * their account by user ID, so it reaches them at once), a person outside it, or `unknown` when
 * the caller may not list the space's people.
 */
export type EmailMatch = { kind: 'me' } | { kind: 'person'; member: Schemas['Member'] } | { kind: 'outside' } | { kind: 'unknown' }

/**
 * The access a person of a team space has everywhere in it through the team root: owners and admins
 * manage, a member has their content role (decision 0003-1, default manage), a guest nothing.
 */
export function teamAccess(m: { role: Schemas['MemberRole']; content_role?: Schemas['ContentRole'] | null }): Schemas['Role'] | null {
  if (m.role === 'owner' || m.role === 'admin') return 'manage'
  if (m.role === 'member') return m.content_role ?? 'manage'
  return null
}

/** True when `role` gives nothing more than `base` (roles are ordered view < comment < edit < manage). */
const covers = (base: Schemas['Role'] | null, role: Schemas['Role']) => base !== null && roles.indexOf(base) >= roles.indexOf(role)

/** The rows of the access from the space: one per access, with the people in it (S2, decision 0003-1). */
type TeamRow = { access: Schemas['Role']; admins: number; members: number }

function teamRows(people: Schemas['Member'][]): TeamRow[] {
  const by = new Map<Schemas['Role'], TeamRow>()
  for (const m of people) {
    const access = teamAccess(m)
    if (!access) continue
    const row = by.get(access) ?? { access, admins: 0, members: 0 }
    if (m.role === 'member') row.members++
    else row.admins++
    by.set(access, row)
  }
  return roles
    .filter((r) => by.has(r))
    .reverse()
    .map((r) => by.get(r)!)
}

/** The share dialog: people with access, the link, and who may share further (copy.md 3.3). */
export function ShareDialog(props: { node: Node; open: boolean; onClose: () => void; onChanged: () => void }) {
  const { node } = props
  const { tenant, role: myTenantRole, isAdmin } = useTenant()
  const { me } = useSession()
  const names = useNames()
  const actorText = useActorText()
  const policy = tenant.policy
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const grants = usePagedApi(
    (cursor) =>
      unwrap(client.GET('/v1/nodes/{node_id}/grants', { params: { path: { node_id: node.id }, query: cursor ? { cursor } : {} } })),
    [node.id],
  )
  const links = usePagedApi(
    (cursor) =>
      unwrap(client.GET('/v1/nodes/{node_id}/share-links', { params: { path: { node_id: node.id }, query: cursor ? { cursor } : {} } })),
    [node.id],
  )
  // Who is in the space. A share with a person outside it does not let them open the item: they
  // must also join the space as a guest (docs/STATUS.md, the 404 for a grant without membership).
  const people = useApi(
    () => (myTenantRole === 'guest' ? Promise.resolve(null) : loadSpacePeople(tenant.id).catch(() => null)),
    [tenant.id, myTenantRole],
  )
  const canInviteGuests = isAdmin && policy.allow_guests
  const team = tenant.kind === 'team'
  const match = (email: string): EmailMatch => {
    // i18n-ignore: email addresses compared without case
    const e = email.trim().toLowerCase()
    // i18n-ignore: email addresses compared without case
    if (e === me.user.email.toLowerCase()) return { kind: 'me' }
    const known = people.data
    if (!known) return { kind: 'unknown' }
    const member = known.byEmail.get(e)
    return member ? { kind: 'person', member } : { kind: 'outside' }
  }
  // Owners and admins of a team space manage everything in it through the team root; members have
  // their content role there (decision 0003-1). A share or a sharing rule can't give them less, so
  // the dialog shows that access too, one row per access.
  const spacePeople = people.data ? [...people.data.byId.values()] : null
  const rows = spacePeople ? teamRows(spacePeople) : null
  const myAccess = team ? teamAccess({ role: myTenantRole, content_role: spacePeople?.find((m) => m.user_id === me.user.id)?.content_role }) : null

  // The folders that inherited grants come from, by name (ia.md: "from folder Name").
  const fromIds = [...new Set((grants.data?.items ?? []).map((g) => g.inherited_from).filter((x): x is string => Boolean(x)))]
  const folders = useApi(async () => {
    const out: Record<string, string> = {}
    for (const id of fromIds) {
      try {
        const n = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: id } } }))
        out[id] = n.parent_id ? n.name : t('shell.nav.files')
      } catch {
        // A folder the person cannot see: the row says "from a folder above".
      }
    }
    return out
  }, [fromIds.join(',')])

  // Grantees with a name or email teach the name directory.
  useEffect(() => {
    for (const g of grants.data?.items ?? []) {
      if (g.grantee.type === 'user' && (g.grantee.display_name || g.grantee.email)) {
        names.learn(g.grantee.id, { name: g.grantee.display_name ?? g.grantee.email!, email: g.grantee.email })
      }
    }
  }, [grants.data, names])

  // The value "Same as the folder above" stands for: the parent's effective rule, or yes at the top.
  const parentRule = useApi(async () => {
    if (node.kind !== 'folder') return null
    if (!node.parent_id) return { top: true, value: true }
    try {
      const parent = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: node.parent_id } } }))
      return { top: !parent.parent_id, value: parent.editors_can_share_effective ?? true }
    } catch {
      return null
    }
  }, [node.id, node.parent_id, node.kind])
  const [ruleSaved, setRuleSaved] = useState(false)

  const canShare = node.my_can_share ?? node.my_role === 'manage'
  const canManage = node.my_role === 'manage'
  const grantable = grantableRoles(node.my_role)
  // Share links never give manage, and never more than the caller has.
  const linkGrantable = linkRoles.filter((r) => grantable.includes(r))
  const owner = names.person(node.owner_id)
  const nameOf = (g: Schemas['Grant']) => granteeName(g.grantee, names, tenant.name, g.id)

  async function run(action: () => Promise<unknown>, done: string, reload: () => void): Promise<boolean> {
    setError(undefined)
    try {
      await action()
      setNotice(done)
      reload()
      return true
    } catch (e) {
      setError(e)
      return false
    }
  }

  return (
    <Dialog title={t('share.title', { name: node.name })} open={props.open} onClose={props.onClose} size="md">
      <Stack>
        <ErrorNote error={error} context={{ share: true, space: tenant.name }} />
        <Notice text={notice} />
        {canShare ? (
          <AddPersonForm
            roles={grantable}
            spaceName={tenant.name}
            team={team}
            membersCanShare={policy.members_can_share}
            itemName={node.name}
            match={match}
            allowGuests={policy.allow_guests}
            guestCaller={myTenantRole === 'guest'}
            isAdmin={isAdmin}
            tenantId={tenant.id}
            onUseLink={
              !policy.allow_share_links || linkGrantable.length === 0
                ? undefined
                : () => {
                    const select = document.getElementById('link-role')
                    select?.scrollIntoView?.({ block: 'center' })
                    select?.focus()
                  }
            }
            onAdd={async (target, role, notify, message) => {
              setError(undefined)
              setNotice(null)
              // A person of the space gets the share by user ID: it reaches their account at once
              // (decision 0002-1). Anyone else gets an email grant; it binds when they sign in with
              // that address (decision 0003-2). Ruum emails them when "Notify people" is on (0003-3).
              const grantee: Schemas['Grantee'] =
                target.kind === 'person'
                  ? { type: 'user', id: target.member.user_id }
                  : target.kind === 'everyone'
                    ? { type: 'tenant_members' }
                    : { type: 'email', email: target.email }
              const body: Schemas['AddGrantRequest'] = { grantee, role, notify, ...(notify && message ? { message } : {}) }
              const who =
                target.kind === 'person'
                  ? target.member.display_name || target.member.email
                  : target.kind === 'everyone'
                    ? t('share.everyone', { space: tenant.name })
                    : target.email
              let res: Schemas['GrantWrite']
              try {
                res = await unwrap(client.POST('/v1/nodes/{node_id}/grants', { params: { path: { node_id: node.id } }, body }))
                if (target.kind === 'person') names.learn(target.member.user_id, { name: who, email: target.member.email })
                else if (target.kind === 'email') rememberGrantEmail(res.grant.id, target.email)
              } catch (e) {
                setError(e)
                return false
              }
              grants.reload()
              const access = target.kind === 'person' && team ? teamAccess(target.member) : null
              const base =
                access && covers(access, role)
                  ? t('share.done.role', { name: node.name, who, role: roleLabels[access], space: tenant.name })
                  : t('share.done', { name: node.name, who })
              const mail = !notify || target.kind === 'everyone' ? '' : res.notified ? t('share.done.mailed') : t('share.done.notMailed')
              setNotice(mail ? `${base} ${mail}` : base)
              return true
            }}
          />
        ) : (
          <Text tone="muted">{owner?.name ? t('share.cannot', { owner: owner.name }) : t('share.cannot.owner')}</Text>
        )}

        <section aria-labelledby="share-people">
          <h3 id="share-people">{t('share.people')}</h3>
          <Loaded state={grants}>
            {(page) => (
              <List>
                <ListRow>
                  <Grow>
                    {owner?.name ?? t('share.theOwner')}
                    {owner?.email && (
                      <Text as="span" tone="muted" size="sm" block>
                        {owner.email}
                      </Text>
                    )}
                  </Grow>
                  <Text as="span" tone="muted">
                    {t('memberRole.owner')}
                  </Text>
                </ListRow>
                {team &&
                  (rows && rows.length > 1 ? rows : [{ access: 'manage' as const, admins: 0, members: 0 }]).map((r, _i, all) => (
                    <ListRow key={r.access}>
                      <Grow>
                        {all.length === 1
                          ? t('share.roleRow', { space: tenant.name })
                          : t('share.roleRow.access', { space: tenant.name, admins: r.admins, members: r.members, both: r.admins && r.members ? 'yes' : 'no' })}
                        {all.length === 1 && (
                          <Text as="span" tone="muted" size="sm" block>
                            {t('share.roleRow.detail', {
                              hasCount: rows ? 'yes' : 'no',
                              n: rows?.[0] ? rows[0].admins + rows[0].members : 0,
                              space: tenant.name,
                              share: policy.members_can_share ? 'yes' : 'no',
                            })}
                          </Text>
                        )}
                      </Grow>
                      <Text as="span" tone="muted">
                        {roleLabels[r.access]}
                      </Text>
                    </ListRow>
                  ))}
                {team && myAccess && (
                  <ListRow>
                    <Grow>
                      {t('share.yourAccess')}
                      <Text as="span" tone="muted" size="sm" block>
                        {t('share.yourAccess.detail', { space: tenant.name, role: memberRoleInline[myTenantRole] })}
                      </Text>
                    </Grow>
                    <Text as="span" tone="muted">
                      {roleLabels[myAccess]}
                    </Text>
                  </ListRow>
                )}
                {!team && page.items.length === 0 && (
                  <Text as="li" tone="muted">
                    {t('share.onlyYou')}
                  </Text>
                )}
                {page.items.map((g) => {
                  const name = nameOf(g)
                  const detail = granteeDetail(g.grantee, names, g.id)
                  const mine = g.grantee.type === 'user' && g.grantee.id === me.user.id
                  const member = g.grantee.type === 'user' ? people.data?.byId.get(g.grantee.id) : undefined
                  // A person outside the space can't open the item until they join it.
                  const notInSpace = g.grantee.type === 'user' && people.data && !mine && !member
                  // A share no stronger than the person's access from the space changes nothing for them.
                  const memberAccess = team && member ? teamAccess(member) : null
                  const roleWins = Boolean(memberAccess && covers(memberAccess, g.role) && !mine)
                  const addedBy = !g.inherited_from && g.created_by.actor.id !== node.owner_id ? actorText(g.created_by) : null
                  return (
                    <ListRow key={g.id}>
                      <Grow>
                        {name}
                        {mine && ` ${t('share.you')}`}
                        {detail && (
                          <Text as="span" tone="muted" size="sm" block>
                            {detail}
                          </Text>
                        )}
                        {addedBy && (
                          <Text as="span" tone="muted" size="sm" block>
                            {g.created_by.actor.id === me.user.id ? t('share.addedBy.you') : t('share.addedBy', { who: addedBy })}
                          </Text>
                        )}
                        {roleWins && (
                          <Text as="span" size="sm" block>
                            {t('share.roleWins.access', { role: roleLabels[memberAccess!], space: tenant.name })}
                          </Text>
                        )}
                        {notInSpace && (
                          <Text as="span" size="sm" block>
                            {canInviteGuests ? t('share.notJoined.invite') : t('share.notJoined.ask')}
                          </Text>
                        )}
                        {g.inherited_from && (
                          <Text as="span" tone="muted" size="sm" block>
                            {folders.data?.[g.inherited_from]
                              ? tr('share.fromFolder', {
                                  folder: folders.data[g.inherited_from],
                                  link: (chunks) => (
                                    <Link to={tenantPath(node.tenant_id, 'files', g.inherited_from!)} onClick={props.onClose}>
                                      {chunks}
                                    </Link>
                                  ),
                                })
                              : t('share.fromAbove')}
                          </Text>
                        )}
                      </Grow>
                      {canShare && !mine && !g.inherited_from && grantable.includes(g.role) ? (
                        <Menu
                          label={roleLabels[g.role]}
                          context={t('share.accessOf', { name })}
                          align="end"
                          size="sm"
                          items={[
                            ...grantable.map((r) => ({
                              label: roleLabels[r],
                              hint: roleMeanings[r],
                              checked: g.role === r,
                              onSelect: () => {
                                if (r === g.role) return
                                void run(
                                  () =>
                                    unwrap(
                                      client.PATCH('/v1/nodes/{node_id}/grants/{grant_id}', {
                                        params: { path: { node_id: node.id, grant_id: g.id } },
                                        body: { role: r },
                                      }),
                                    ),
                                  t('share.changed', { name, role: roleInline[r] }),
                                  grants.reload,
                                )
                              },
                            })),
                            {
                              label: t('share.removeAccess'),
                              danger: true,
                              onSelect: () =>
                                void run(
                                  () =>
                                    unwrap(
                                      client.DELETE('/v1/nodes/{node_id}/grants/{grant_id}', {
                                        params: { path: { node_id: node.id, grant_id: g.id } },
                                      }),
                                    ),
                                  t('share.removed', { name }),
                                  grants.reload,
                                ),
                            },
                          ]}
                        />
                      ) : (
                        <Text as="span" tone="muted">
                          {roleLabels[g.role]}
                        </Text>
                      )}
                    </ListRow>
                  )
                })}
              </List>
            )}
          </Loaded>
          <LoadMore state={grants} />
        </section>

        <section aria-labelledby="share-links">
          <h3 id="share-links">{t('share.link')}</h3>
          {!policy.allow_share_links ? (
            <Text tone="muted">{t('share.link.off')}</Text>
          ) : (
            <>
              <Loaded state={links}>
                {(page) => {
                  // The list has turned-off links too; only live ones are shown.
                  const live = page.items.filter((l) => !l.revoked_at)
                  return (
                    <List>
                      {live.length === 0 && (
                        <Text as="li" tone="muted">
                          {t('share.link.none')}
                        </Text>
                      )}
                      {live.map((l) => (
                        <ListRow key={l.id}>
                          <Grow>
                            {t('share.link.role', { role: roleInline[l.role] })}
                            <Text as="span" tone="muted" size="sm" block>
                              {t('share.link.ends', {
                                hasEnd: l.expires_at ? 'yes' : 'no',
                                date: l.expires_at ? formatDay(l.expires_at) : '',
                                password: l.has_password ? 'yes' : 'no',
                              })}
                            </Text>
                          </Grow>
                          {canShare && (
                            <ConfirmButton
                              label={t('account.appPassword.off')}
                              ariaLabel={t('share.link.turnOff.label', { role: roleInline[l.role] })}
                              title={t('share.link.turnOff.title')}
                              message={t('share.link.turnOff.message')}
                              confirmLabel={t('share.link.turnOff.confirm')}
                              danger
                              onConfirm={() =>
                                run(
                                  () => unwrap(client.DELETE('/v1/share-links/{link_id}', { params: { path: { link_id: l.id } } })),
                                  t('share.link.turnedOff'),
                                  links.reload,
                                )
                              }
                            />
                          )}
                        </ListRow>
                      ))}
                    </List>
                  )
                }}
              </Loaded>
              <LoadMore state={links} />
              {canShare && linkGrantable.length > 0 && (
                <CreateLinkForm
                  roles={linkGrantable}
                  allowEdit={policy.allow_edit_links}
                  maxDays={policy.share_link_max_days}
                  onCreate={async (body) => {
                    const res = await unwrap(
                      client.POST('/v1/nodes/{node_id}/share-links', { params: { path: { node_id: node.id } }, body }),
                    )
                    links.reload()
                    return res.share_link
                  }}
                />
              )}
            </>
          )}
        </section>

        {canManage && node.kind === 'folder' && (
          <section aria-labelledby="share-settings">
            <h3 id="share-settings">{t('share.rule')}</h3>
            <Text tone="muted" size="sm" id="share-rule-help">
              {tr('share.rule.help', {
                role: roleLabels.edit,
                team: team ? 'yes' : 'no',
                space: tenant.name,
                b: (chunks) => <strong>{chunks}</strong>,
              })}
            </Text>
            <Field label={t('share.rule.label')}>
              <Select
                aria-describedby="share-rule-help"
                onFocus={() => setRuleSaved(false)}
                value={node.editors_can_share === null || node.editors_can_share === undefined ? 'inherit' : String(node.editors_can_share)}
                onChange={(e) =>
                  run(
                    () =>
                      unwrap(
                        client.PATCH('/v1/nodes/{node_id}/sharing', {
                          params: { path: { node_id: node.id } },
                          body: { editors_can_share: e.target.value === 'inherit' ? null : e.target.value === 'true' },
                        }),
                      ),
                    t('share.rule.saved'),
                    props.onChanged,
                  ).then(setRuleSaved)
                }
              >
                <option value="inherit">
                  {t('share.rule.inherit', {
                    top: parentRule.data?.top ? 'yes' : 'no',
                    value: (parentRule.data?.value ?? true) ? 'yes' : 'no',
                  })}
                </option>
                <option value="true">{t('common.yes')}</option>
                <option value="false">{t('common.no')}</option>
              </Select>
            </Field>
            {ruleSaved && (
              <Text tone="muted" size="sm" role="status">
                {t('common.saved')}
              </Text>
            )}
          </section>
        )}
      </Stack>
    </Dialog>
  )
}

/** Whom the add form shares with: a person of the space (by user ID), an email, or everyone in the space. */
type AddTarget = { kind: 'person'; member: Schemas['Member'] } | { kind: 'email'; email: string } | { kind: 'everyone' }

/** The longest personal message of a share email (contract, `AddGrantRequest.message`). */
const MESSAGE_MAX = 1000

function AddPersonForm(props: {
  onAdd: (target: AddTarget, role: Schemas['Role'], notify: boolean, message: string) => Promise<boolean>
  onUseLink?: () => void
  roles: Schemas['Role'][]
  spaceName: string
  team: boolean
  membersCanShare: boolean
  itemName: string
  match: (email: string) => EmailMatch
  allowGuests: boolean
  guestCaller: boolean
  isAdmin: boolean
  tenantId: string
}) {
  const { roles, spaceName } = props
  const [email, setEmail] = useState('')
  const [everyone, setEveryone] = useState(false)
  const [role, setRole] = useState<Schemas['Role']>('view')
  const [notify, setNotify] = useState(true)
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const valid = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())
  const found: EmailMatch | null = !everyone && valid ? props.match(email) : null
  const person = found?.kind === 'person' ? found.member : null
  const outside = found?.kind === 'outside'
  // A guest who shares sees every email as an email grant (decision 0003-2, rule a').
  const emailGrant = !everyone && valid && !person && found?.kind !== 'me'
  const access = props.team && person ? teamAccess(person) : null
  const roleWins = access !== null && covers(access, role)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    try {
      const target: AddTarget = everyone ? { kind: 'everyone' } : person ? { kind: 'person', member: person } : { kind: 'email', email: email.trim() }
      const ok = await props.onAdd(target, role, notify && !everyone, message.trim())
      if (ok) {
        setEmail('')
        setMessage('')
        setEveryone(false)
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <Stack as="form" onSubmit={submit} aria-label={t('share.add')}>
      <Row align="end">
        {everyone ? (
          <Grow>
            <Text>{t('share.everyone', { space: spaceName })}</Text>
          </Grow>
        ) : (
          <Field label={t('account.email')} grow>
            <TextInput
              type="email"
              required
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              placeholder={t('share.email.placeholder')}
              aria-describedby={emailGrant ? 'add-email-help' : undefined}
            />
          </Field>
        )}
        <Field label={t('share.access')}>
          <Select width="auto" value={role} onChange={(e) => setRole(e.target.value as Schemas['Role'])} aria-describedby="add-role-meaning">
            {roles.map((r) => (
              <option key={r} value={r}>
                {roleLabels[r]}
              </option>
            ))}
          </Select>
        </Field>
        <Button variant="primary" type="submit" disabled={busy || found?.kind === 'me'} busy={busy}>
          {t('details.share')}
        </Button>
      </Row>
      <Text tone="muted" size="sm" id="add-role-meaning">
        {t('share.roleMeaning', { role: roleLabels[role], meaning: roleMeanings[role] })}
      </Text>
      {props.team && !props.guestCaller && (
        <Text tone="muted" size="sm">
          <Button variant="link" onClick={() => setEveryone((x) => !x)}>
            {everyone ? t('share.everyone.back') : t('share.everyone.pick', { space: spaceName })}
          </Button>
        </Text>
      )}
      {everyone && (
        <Note role="note">{t('share.everyone.note', { space: spaceName, role: roleLabels[role] })}</Note>
      )}
      {found?.kind === 'me' && (
        <Text tone="muted" size="sm">
          {t('share.ownEmail')}
        </Text>
      )}
      {person &&
        (roleWins ? (
          <Note tone="warning" role="note">
            {tr('share.add.roleWins.access', {
              name: person.display_name || person.email,
              access: roleLabels[access!],
              space: spaceName,
              admin: props.isAdmin ? 'yes' : 'no',
              link: (chunks) => <Link to={tenantPath(props.tenantId, 'admin', 'members')}>{chunks}</Link>,
            })}
          </Note>
        ) : (
          <Text tone="muted" size="sm">
            {t('share.add.inSpace', { name: person.display_name || person.email, space: spaceName, item: props.itemName })}
          </Text>
        ))}
      {emailGrant && (
        <Text tone="muted" size="sm" id="add-email-help">
          {t('share.add.emailHelp')}
        </Text>
      )}
      {outside &&
        (props.allowGuests ? (
          <Text tone="muted" size="sm">
            {t('share.add.outside', { email: email.trim(), item: props.itemName, space: spaceName })}
          </Text>
        ) : (
          <Note tone="warning" role="note">
            {t('share.add.guestsOff', { email: email.trim(), space: spaceName })}
          </Note>
        ))}
      {!everyone && (
        <Stack gap={2}>
          <Checkbox checked={notify} onChange={(e) => setNotify(e.target.checked)} label={t('share.notify')} />
          {notify && (
            <Field
              label={t('share.message')}
              hint={message.length > MESSAGE_MAX - 100 ? t('share.message.count', { n: message.length, max: MESSAGE_MAX }) : undefined}
              hintId="share-message-count"
            >
              <TextArea
                rows={2}
                maxLength={MESSAGE_MAX}
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                aria-describedby={message.length > MESSAGE_MAX - 100 ? 'share-message-count' : undefined}
              />
            </Field>
          )}
        </Stack>
      )}
      {outside && props.onUseLink && (
        <Text tone="muted" size="sm">
          {tr('share.add.useLink', {
            item: props.itemName,
            button: (chunks) => (
              <Button variant="link" onClick={props.onUseLink}>
                {chunks}
              </Button>
            ),
          })}
        </Text>
      )}
    </Stack>
  )
}

function CreateLinkForm(props: {
  roles: Schemas['ShareLinkRole'][]
  allowEdit: boolean
  maxDays: number | null
  onCreate: (body: Schemas['CreateShareLinkRequest']) => Promise<Schemas['ShareLink']>
}) {
  const [role, setRole] = useState<Schemas['ShareLinkRole']>('view')
  const [days, setDays] = useState<string>(String(props.maxDays ? Math.min(7, props.maxDays) : 7))
  const [password, setPassword] = useState('')
  const [created, setCreated] = useState<Schemas['ShareLink'] | null>(null)
  const [error, setError] = useState<unknown>()

  async function submit(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      const n = Number(days)
      const link = await props.onCreate({
        role,
        ...(n > 0 ? { expires_at: new Date(Date.now() + n * 86400_000).toISOString() } : {}),
        ...(password ? { password } : {}),
      })
      setCreated(link)
      setPassword('')
    } catch (err) {
      setError(err)
    }
  }

  return (
    <Stack as="form" onSubmit={submit} aria-label={t('share.link.create.label')}>
      <Row align="end">
        <Field label={t('share.access')}>
          <Select id="link-role" value={role} onChange={(e) => setRole(e.target.value as Schemas['ShareLinkRole'])}>
            {props.roles
              .filter((r) => r !== 'edit' || props.allowEdit)
              .map((r) => (
                <option key={r} value={r}>
                  {roleLabels[r]}
                </option>
              ))}
          </Select>
        </Field>
        <Field label={t('share.link.days')}>
          <TextInput
            type="number"
            min={1}
            max={props.maxDays ?? undefined}
            required={props.maxDays !== null}
            value={days}
            onChange={(e) => setDays(e.target.value)}
          />
        </Field>
        <Field label={t('share.link.password')}>
          <TextInput type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <Button type="submit">{t('share.link.create')}</Button>
      </Row>
      <Text tone="muted" size="sm">
        {t('share.link.once')}
      </Text>
      <ErrorNote error={error} />
      {created?.url && <SecretBox label={t('share.link.new')} secret={created.url} note={t('share.link.new.note')} />}
    </Stack>
  )
}
