import { useEffect, useState, type FormEvent } from 'react'
import { Link } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useActorText, useNames, type Names } from '../../app/useNames'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDay, memberRoleLabels, roleLabels, roleMeanings } from '../../components/format'
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
  Note,
  Notice,
  Row,
  SecretBox,
  Select,
  Stack,
  Text,
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
      if (m.email) byEmail.set(m.email.toLowerCase(), m)
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

/** True for the space roles that manage everything in a team space (decision 0001-4, team root). */
const managesEverything = (role: Schemas['MemberRole']) => role !== 'guest'

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
    const e = email.trim().toLowerCase()
    if (e === me.user.email.toLowerCase()) return { kind: 'me' }
    const known = people.data
    if (!known) return { kind: 'unknown' }
    const member = known.byEmail.get(e)
    return member ? { kind: 'person', member } : { kind: 'outside' }
  }
  // Members, admins and owners of a team space manage everything in it through the team root,
  // and share there while the policy `members_can_share` is on. A share or a sharing rule can't
  // give them less, so the dialog shows that access too.
  const roleCount = people.data ? [...people.data.byId.values()].filter((m) => managesEverything(m.role)).length : null

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
        <ErrorNote error={error} />
        <Notice text={notice} />
        {canShare ? (
          <AddPersonForm
            roles={grantable}
            spaceName={tenant.name}
            team={team}
            membersCanShare={policy.members_can_share}
            itemName={node.name}
            match={match}
            canInvite={canInviteGuests}
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
            onAdd={async (target, role, inviteGuest) => {
              setError(undefined)
              // A person of the space gets the share by user ID: it reaches their account at once
              // (decision 0002-1). An email grant would wait for an invitation that never comes.
              const body: Schemas['AddGrantRequest'] =
                target.kind === 'person'
                  ? { grantee: { type: 'user', id: target.member.user_id }, role, notify: false }
                  : { grantee: { type: 'email', email: target.email }, role, notify: false }
              const who = target.kind === 'person' ? target.member.display_name || target.member.email : target.email
              try {
                const res = await unwrap(client.POST('/v1/nodes/{node_id}/grants', { params: { path: { node_id: node.id } }, body }))
                if (target.kind === 'person') names.learn(target.member.user_id, { name: who, email: target.member.email })
                else rememberGrantEmail(res.grant.id, target.email)
              } catch (e) {
                setError(e)
                return
              }
              grants.reload()
              if (target.kind === 'person') {
                setNotice(
                  team && managesEverything(target.member.role)
                    ? t('share.done.role', {
                        name: node.name,
                        who,
                        role: memberRoleLabels[target.member.role].toLowerCase(),
                        space: tenant.name,
                      })
                    : t('share.done.person', { name: node.name, who }),
                )
                return
              }
              if (!inviteGuest) {
                setNotice(t('share.done.noInvite', { name: node.name, who }))
                return
              }
              // The person must join before they can open the item: Ruum emails them an invitation.
              try {
                await unwrap(
                  client.POST('/v1/tenants/{tenant_id}/invitations', {
                    params: { path: { tenant_id: tenant.id } },
                    body: {
                      email: target.email,
                      role: 'guest',
                      message: t('share.invitation.message', { name: node.name }),
                    },
                  }),
                )
                setNotice(t('share.done.invited', { name: node.name, who }))
              } catch (e) {
                setNotice(t('share.done.inviteFailed', { name: node.name, who }))
                setError(e)
              }
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
                {team && (
                  <ListRow>
                    <Grow>
                      {t('share.roleRow', { space: tenant.name })}
                      <Text as="span" tone="muted" size="sm" block>
                        {t('share.roleRow.detail', {
                          hasCount: roleCount !== null ? 'yes' : 'no',
                          n: roleCount ?? 0,
                          space: tenant.name,
                          share: policy.members_can_share ? 'yes' : 'no',
                        })}
                      </Text>
                    </Grow>
                    <Text as="span" tone="muted">
                      {roleLabels.manage}
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
                  // A share weaker than the space role changes nothing for that person.
                  const roleWins = team && member && managesEverything(member.role) && !mine
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
                            {t('share.roleWins', { role: memberRoleLabels[member.role], space: tenant.name })}
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
                        <>
                          <Select
                            aria-label={t('share.accessOf', { name })}
                            value={g.role}
                            onChange={(e) =>
                              run(
                                () =>
                                  unwrap(
                                    client.PATCH('/v1/nodes/{node_id}/grants/{grant_id}', {
                                      params: { path: { node_id: node.id, grant_id: g.id } },
                                      body: { role: e.target.value as Schemas['Role'] },
                                    }),
                                  ),
                                t('share.changed', { name, role: roleLabels[e.target.value as Schemas['Role']].toLowerCase() }),
                                grants.reload,
                              )
                            }
                          >
                            {grantable.map((r) => (
                              <option key={r} value={r}>
                                {roleLabels[r]}
                              </option>
                            ))}
                          </Select>
                          <Button
                            aria-label={t('share.remove.label', { name })}
                            onClick={() =>
                              run(
                                () =>
                                  unwrap(
                                    client.DELETE('/v1/nodes/{node_id}/grants/{grant_id}', {
                                      params: { path: { node_id: node.id, grant_id: g.id } },
                                    }),
                                  ),
                                t('share.removed', { name }),
                                grants.reload,
                              )
                            }
                          >
                            {t('share.remove')}
                          </Button>
                        </>
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
                            {t('share.link.role', { role: roleLabels[l.role].toLowerCase() })}
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
                              ariaLabel={t('share.link.turnOff.label', { role: roleLabels[l.role].toLowerCase() })}
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

/** Whom the add form shares with: a person of the space (by user ID) or an email. */
type AddTarget = { kind: 'person'; member: Schemas['Member'] } | { kind: 'email'; email: string }

function AddPersonForm(props: {
  onAdd: (target: AddTarget, role: Schemas['Role'], inviteGuest: boolean) => Promise<void>
  onUseLink?: () => void
  roles: Schemas['Role'][]
  spaceName: string
  team: boolean
  membersCanShare: boolean
  itemName: string
  match: (email: string) => EmailMatch
  canInvite: boolean
  isAdmin: boolean
  tenantId: string
}) {
  const { roles, spaceName } = props
  const [email, setEmail] = useState('')
  const [role, setRole] = useState<Schemas['Role']>('view')
  const [invite, setInvite] = useState(true)
  const [busy, setBusy] = useState(false)
  const valid = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())
  const found: EmailMatch | null = valid ? props.match(email) : null
  const person = found?.kind === 'person' ? found.member : null
  const outside = found?.kind === 'outside'
  const roleWins = props.team && person !== null && managesEverything(person.role)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    try {
      // `notify: false`: the server sends no share email yet (crates/ruum-api/src/routes/sharing.rs).
      // A person outside the space gets the invitation email instead (the box below).
      const target: AddTarget = person ? { kind: 'person', member: person } : { kind: 'email', email: email.trim() }
      await props.onAdd(target, role, outside && props.canInvite && invite)
      setEmail('')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Stack as="form" onSubmit={submit} aria-label={t('share.add')}>
      <Row align="end">
        <Field label={t('account.email')} grow>
          <TextInput
            type="email"
            required
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            placeholder={t('share.email.placeholder')}
          />
        </Field>
        <Field label={t('share.access')}>
          <Select value={role} onChange={(e) => setRole(e.target.value as Schemas['Role'])} aria-describedby="add-role-meaning">
            {roles.map((r) => (
              <option key={r} value={r}>
                {roleLabels[r]}
              </option>
            ))}
          </Select>
        </Field>
        <Button variant="primary" type="submit" disabled={busy || found?.kind === 'me'}>
          {t('details.share')}
        </Button>
      </Row>
      <Text tone="muted" size="sm" id="add-role-meaning">
        {t('share.roleMeaning', { role: roleLabels[role], meaning: roleMeanings[role] })}
      </Text>
      {found?.kind === 'me' && (
        <Text tone="muted" size="sm">
          {t('share.ownEmail')}
        </Text>
      )}
      {person &&
        (roleWins ? (
          <Note tone="warning" role="note">
            {tr('share.add.roleWins', {
              name: person.display_name || person.email,
              role: person.role,
              space: spaceName,
              share: props.membersCanShare || person.role !== 'member' ? 'yes' : 'no',
              admin: props.isAdmin ? 'yes' : 'no',
              link: (chunks) => <Link to={tenantPath(props.tenantId, 'admin', 'members')}>{chunks}</Link>,
            })}
          </Note>
        ) : (
          <Text tone="muted" size="sm">
            {t('share.add.inSpace', { name: person.display_name || person.email, space: spaceName, item: props.itemName })}
          </Text>
        ))}
      {outside &&
        (props.canInvite ? (
          <Stack>
            <Checkbox
              checked={invite}
              onChange={(e) => setInvite(e.target.checked)}
              aria-describedby="add-outside"
              label={t('share.add.invite')}
            />
            <Text tone="muted" size="sm" id="add-outside">
              {t('share.add.new', { email: email.trim(), item: props.itemName })}
              {invite ? '' : ` ${t('share.add.new.without')}`}
            </Text>
          </Stack>
        ) : (
          <Note tone="warning" role="note">
            {t('share.add.new.admin', { email: email.trim(), item: props.itemName, space: spaceName })}
          </Note>
        ))}
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
