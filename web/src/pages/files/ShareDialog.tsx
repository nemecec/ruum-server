import { useEffect, useState, type FormEvent } from 'react'
import { Link } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useActorText, useNames, type Names } from '../../app/useNames'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { count, memberRoleLabels, roleLabels, roleMeanings } from '../../components/format'
import { ConfirmButton, Dialog, ErrorNote, Loaded, LoadMore, Notice, SecretBox } from '../../components/ui'
import type { Node } from './api'

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
      return g.display_name ?? person?.name ?? g.email ?? (grantId ? grantEmails()[grantId] : undefined) ?? 'A person outside this space'
    }
    case 'email':
      return g.email
    case 'tenant_members':
      return `Everyone in ${spaceName}`
  }
}

/** The second line of a grantee: the email under a name, or "Invited, not joined yet". */
function granteeDetail(g: Schemas['Grantee'], names: Names, grantId: string): string | null {
  if (g.type === 'email') return 'Invited, not joined yet'
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
export type EmailMatch =
  | { kind: 'me' }
  | { kind: 'person'; member: Schemas['Member'] }
  | { kind: 'outside' }
  | { kind: 'unknown' }

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
  const folders = useApi(
    async () => {
      const out: Record<string, string> = {}
      for (const id of fromIds) {
        try {
          const n = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: id } } }))
          out[id] = n.parent_id ? n.name : 'Files'
        } catch {
          // A folder the person cannot see: the row says "from a folder above".
        }
      }
      return out
    },
    [fromIds.join(',')],
  )

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
    <Dialog title={`Share ${node.name}`} open={props.open} onClose={props.onClose} wide>
      <div className="stack">
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
                    ? `Shared ${node.name} with ${who}. As ${memberRoleLabels[target.member.role].toLowerCase()} of ${tenant.name}, they could already manage it.`
                    : `Shared ${node.name} with ${who}. They find it under Shared with me.`,
                )
                return
              }
              if (!inviteGuest) {
                setNotice(`Shared ${node.name} with ${who}. They can open it only after they join; nobody emailed them.`)
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
                      message: `I shared "${node.name}" with you. Accept this invitation and make a free Ruum account to open it.`,
                    },
                  }),
                )
                setNotice(`Shared ${node.name} with ${who}. Ruum emailed them an invitation: they make a free account, then they can open it.`)
              } catch (e) {
                setNotice(`Shared ${node.name} with ${who}, but the invitation email was not sent.`)
                setError(e)
              }
            }}
          />
        ) : (
          <p className="muted">You can see who has access, but you can't share this. Ask {owner?.name ?? 'the owner'}.</p>
        )}

        <section aria-labelledby="share-people">
          <h3 id="share-people">People with access</h3>
          <Loaded state={grants}>
            {(page) => (
              <ul className="rows">
                <li className="row">
                  <span className="grow">
                    {owner?.name ?? 'The owner'}
                    {owner?.email && <span className="muted small block">{owner.email}</span>}
                  </span>
                  <span className="muted">Owner</span>
                </li>
                {team && (
                  <li className="row">
                    <span className="grow">
                      Everyone in {tenant.name} except guests
                      <span className="muted small block">
                        {roleCount !== null ? `${count(roleCount, 'person', 'people')}. ` : ''}Owners, admins and members manage everything in{' '}
                        {tenant.name}
                        {policy.members_can_share ? ', and share it' : ''}. This comes from their role in the space; a share here can't
                        give them less.
                      </span>
                    </span>
                    <span className="muted">{roleLabels.manage}</span>
                  </li>
                )}
                {!team && page.items.length === 0 && <li className="muted">Only you can see this.</li>}
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
                    <li key={g.id} className="row">
                      <span className="grow">
                        {name}
                        {mine && ' (you)'}
                        {detail && <span className="muted small block">{detail}</span>}
                        {addedBy && <span className="muted small block">added by {g.created_by.actor.id === me.user.id ? 'you' : addedBy}</span>}
                        {roleWins && (
                          <span className="small block">
                            {memberRoleLabels[member.role]} of {tenant.name}: can manage everything here, whatever this share says.
                          </span>
                        )}
                        {notInSpace && (
                          <span className="small block">
                            Can't open it yet: they haven't joined
                            {canInviteGuests ? '. Invite them under Space settings, Invitations.' : '. Ask an admin to invite them.'}
                          </span>
                        )}
                        {g.inherited_from && (
                          <span className="muted small block">
                            {folders.data?.[g.inherited_from] ? (
                              <>
                                from folder{' '}
                                <Link to={tenantPath(node.tenant_id, 'files', g.inherited_from)} onClick={props.onClose}>
                                  {folders.data[g.inherited_from]}
                                </Link>
                              </>
                            ) : (
                              'from a folder above'
                            )}
                          </span>
                        )}
                      </span>
                      {canShare && !mine && !g.inherited_from && grantable.includes(g.role) ? (
                        <>
                          <label className="sr-only" htmlFor={`role-${g.id}`}>
                            Access of {name}
                          </label>
                          <select
                            id={`role-${g.id}`}
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
                                `Changed ${name} to ${roleLabels[e.target.value as Schemas['Role']].toLowerCase()}.`,
                                grants.reload,
                              )
                            }
                          >
                            {grantable.map((r) => (
                              <option key={r} value={r}>
                                {roleLabels[r]}
                              </option>
                            ))}
                          </select>
                          <button
                            type="button"
                            aria-label={`Remove access for ${name}`}
                            onClick={() =>
                              run(
                                () =>
                                  unwrap(
                                    client.DELETE('/v1/nodes/{node_id}/grants/{grant_id}', {
                                      params: { path: { node_id: node.id, grant_id: g.id } },
                                    }),
                                  ),
                                `Removed access for ${name}.`,
                                grants.reload,
                              )
                            }
                          >
                            Remove
                          </button>
                        </>
                      ) : (
                        <span className="muted">{roleLabels[g.role]}</span>
                      )}
                    </li>
                  )
                })}
              </ul>
            )}
          </Loaded>
          <LoadMore state={grants} />
        </section>

        <section aria-labelledby="share-links">
          <h3 id="share-links">Link</h3>
          {!policy.allow_share_links ? (
            <p className="muted">Links are turned off in this space.</p>
          ) : (
            <>
              <Loaded state={links}>
                {(page) => {
                  // The list has turned-off links too; only live ones are shown.
                  const live = page.items.filter((l) => !l.revoked_at)
                  return (
                    <ul className="rows">
                      {live.length === 0 && <li className="muted">Anyone with the link: off</li>}
                      {live.map((l) => (
                        <li key={l.id} className="row">
                          <span className="grow">
                            Anyone with the link: {roleLabels[l.role].toLowerCase()}
                            <span className="muted small block">
                              {l.expires_at ? `Ends ${new Date(l.expires_at).toLocaleDateString(undefined, { day: 'numeric', month: 'short', year: 'numeric' })}` : 'No end date'}
                              {l.has_password ? ', with password' : ''}
                            </span>
                          </span>
                          {canShare && (
                            <ConfirmButton
                              label="Turn off"
                              ariaLabel={`Turn off the ${roleLabels[l.role].toLowerCase()} link`}
                              title="Turn off this link?"
                              message="People who use it lose access at once. You can make a new link later."
                              confirmLabel="Turn off link"
                              danger
                              onConfirm={() =>
                                run(
                                  () => unwrap(client.DELETE('/v1/share-links/{link_id}', { params: { path: { link_id: l.id } } })),
                                  'Link turned off.',
                                  links.reload,
                                )
                              }
                            />
                          )}
                        </li>
                      ))}
                    </ul>
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
            <h3 id="share-settings">Sharing rule</h3>
            <p className="muted small" id="share-rule-help">
              Can people who got <strong>Can edit</strong> on this folder share it with others too? This applies to shares and links, not
              to {team ? `the owners, admins and members of ${tenant.name}: they manage everything, whatever this rule says` : 'you'}.
            </p>
            <label>
              People who can edit can also share
              <select
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
                    'Saved the sharing rule.',
                    props.onChanged,
                  ).then(setRuleSaved)
                }
              >
                <option value="inherit">
                  {parentRule.data?.top ? 'Standard' : 'Same as the folder above'} (now: {(parentRule.data?.value ?? true) ? 'yes' : 'no'})
                </option>
                <option value="true">Yes</option>
                <option value="false">No</option>
              </select>
            </label>
            {ruleSaved && (
              <p className="muted small" role="status">
                Saved.
              </p>
            )}
          </section>
        )}
      </div>
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
    <form className="stack" onSubmit={submit} aria-label="Add people">
      <div className="inline-form">
        <label className="grow">
          Email
          <input type="email" required value={email} onChange={(e) => setEmail(e.target.value)} placeholder="name@example.com" />
        </label>
        <label>
          Access
          <select value={role} onChange={(e) => setRole(e.target.value as Schemas['Role'])} aria-describedby="add-role-meaning">
            {roles.map((r) => (
              <option key={r} value={r}>
                {roleLabels[r]}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" className="primary" disabled={busy || found?.kind === 'me'}>
          Share
        </button>
      </div>
      <p id="add-role-meaning" className="muted small">
        {roleLabels[role]}: {roleMeanings[role]}
      </p>
      {found?.kind === 'me' && <p className="muted small">That's your own email: you have access already.</p>}
      {person &&
        (roleWins ? (
          <p className="warning" role="note">
            {person.display_name || person.email} is {person.role === 'admin' ? 'an' : 'a'} {memberRoleLabels[person.role].toLowerCase()} of{' '}
            {spaceName}, so they can already manage everything here
            {props.membersCanShare || person.role !== 'member' ? ' and share it' : ''}. A share can't give them less. To let them see only
            some folders, make them a guest
            {props.isAdmin ? (
              <>
                {' '}
                under <Link to={tenantPath(props.tenantId, 'admin', 'members')}>Members</Link>
              </>
            ) : (
              ' (ask an admin)'
            )}
            .
          </p>
        ) : (
          <p className="muted small">
            {person.display_name || person.email} is in {spaceName}. They find {props.itemName} under Shared with me; Ruum sends no email
            for this.
          </p>
        ))}
      {outside &&
        (props.canInvite ? (
          <div className="stack">
            <label className="check">
              <input type="checkbox" checked={invite} onChange={(e) => setInvite(e.target.checked)} aria-describedby="add-outside" />
              Email them an invitation
            </label>
            <p id="add-outside" className="muted small">
              {email.trim()} is new here. With the invitation they make a free Ruum account, then they can open {props.itemName}.
              {invite ? '' : ' Without it they cannot open it.'}
            </p>
          </div>
        ) : (
          <p className="warning" role="note">
            {email.trim()} is new here. They can open {props.itemName} only after an admin of {spaceName} invites them.
          </p>
        ))}
      {outside && props.onUseLink && (
        <p className="muted small">
          No account wanted?{' '}
          <button type="button" className="link" onClick={props.onUseLink}>
            Use a link instead
          </button>{' '}
          and send it to them yourself: anyone with the link can open {props.itemName} without signing in.
        </p>
      )}
    </form>
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
    <form className="stack" onSubmit={submit} aria-label="Create a link">
      <div className="inline-form">
        <label>
          Access
          <select id="link-role" value={role} onChange={(e) => setRole(e.target.value as Schemas['ShareLinkRole'])}>
            {props.roles
              .filter((r) => r !== 'edit' || props.allowEdit)
              .map((r) => (
                <option key={r} value={r}>
                  {roleLabels[r]}
                </option>
              ))}
          </select>
        </label>
        <label>
          Ends after (days)
          <input
            type="number"
            min={1}
            max={props.maxDays ?? undefined}
            required={props.maxDays !== null}
            value={days}
            onChange={(e) => setDays(e.target.value)}
          />
        </label>
        <label>
          Password (optional)
          <input type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </label>
        <button type="submit">Create link</button>
      </div>
      <p className="muted small">For safety, Ruum shows a new link only once. Copy it when it appears; you can make a new one at any time.</p>
      <ErrorNote error={error} />
      {created?.url && <SecretBox label="New link" secret={created.url} note="Copy the link now. For safety, Ruum shows it only once." />}
    </form>
  )
}
