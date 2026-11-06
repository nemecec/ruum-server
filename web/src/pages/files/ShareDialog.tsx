import { useEffect, useState, type FormEvent } from 'react'
import { Link } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useNames, type Names } from '../../app/useNames'
import { useSession } from '../../app/useSession'
import { tenantPath, useTenant } from '../../app/tenant'
import { roleLabels, roleMeanings } from '../../components/format'
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

/** The people of a space (members and guests), by user ID and by email; `null` when the caller may not list them. */
type SpacePeople = { ids: Set<string>; emails: Set<string> }

async function loadSpacePeople(tenantId: string): Promise<SpacePeople> {
  const ids = new Set<string>()
  const emails = new Set<string>()
  let cursor: string | undefined
  for (let i = 0; i < 20; i++) {
    const page = await unwrap(
      client.GET('/v1/tenants/{tenant_id}/members', { params: { path: { tenant_id: tenantId }, query: cursor ? { cursor } : {} } }),
    )
    for (const m of page.items) {
      ids.add(m.user_id)
      if (m.email) emails.add(m.email.toLowerCase())
    }
    if (!page.next_cursor) break
    cursor = page.next_cursor
  }
  return { ids, emails }
}

/** The share dialog: people with access, the link, and who may share further (copy.md 3.3). */
export function ShareDialog(props: { node: Node; open: boolean; onClose: () => void; onChanged: () => void }) {
  const { node } = props
  const { tenant, role: myTenantRole, isAdmin } = useTenant()
  const { me } = useSession()
  const names = useNames()
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
  /** True when the email is not a member or guest of the space; `null` when the UI cannot tell. */
  const outside = (email: string): boolean | null => {
    const known = people.data
    if (!known) return null
    const e = email.trim().toLowerCase()
    return e !== me.user.email.toLowerCase() && !known.emails.has(e)
  }

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

  const canShare = node.my_can_share ?? node.my_role === 'manage'
  const canManage = node.my_role === 'manage'
  const grantable = grantableRoles(node.my_role)
  // Share links never give manage, and never more than the caller has.
  const linkGrantable = linkRoles.filter((r) => grantable.includes(r))
  const owner = names.person(node.owner_id)
  const nameOf = (g: Schemas['Grant']) => granteeName(g.grantee, names, tenant.name, g.id)

  async function run(action: () => Promise<unknown>, done: string, reload: () => void) {
    setError(undefined)
    try {
      await action()
      setNotice(done)
      reload()
    } catch (e) {
      setError(e)
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
            outside={outside}
            canInvite={canInviteGuests}
            onAdd={async (body, inviteGuest) => {
              setError(undefined)
              const email = body.grantee.type === 'email' ? body.grantee.email : 'them'
              try {
                const res = await unwrap(client.POST('/v1/nodes/{node_id}/grants', { params: { path: { node_id: node.id } }, body }))
                if (body.grantee.type === 'email') rememberGrantEmail(res.grant.id, body.grantee.email)
              } catch (e) {
                setError(e)
                return
              }
              grants.reload()
              if (!inviteGuest || body.grantee.type !== 'email') {
                setNotice(`Shared with ${email}.`)
                return
              }
              // The person must join the space before they can open the item: invite them as a guest.
              try {
                await unwrap(
                  client.POST('/v1/tenants/{tenant_id}/invitations', {
                    params: { path: { tenant_id: tenant.id } },
                    body: { email: body.grantee.email, role: 'guest', message: `I shared ${node.name} with you.` },
                  }),
                )
                setNotice(`Shared with ${email}. Ruum sent them an invitation to join ${tenant.name} as a guest; they can open it after they accept.`)
              } catch (e) {
                setNotice(`Shared with ${email}.`)
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
                {page.items.length === 0 && <li className="muted">Only you can see this.</li>}
                {page.items.map((g) => {
                  const name = nameOf(g)
                  const detail = granteeDetail(g.grantee, names, g.id)
                  // A person outside the space can't open the item until they join it as a guest.
                  const notInSpace =
                    g.grantee.type === 'user' && people.data && g.grantee.id !== me.user.id && !people.data.ids.has(g.grantee.id)
                  return (
                    <li key={g.id} className="row">
                      <span className="grow">
                        {name}
                        {detail && <span className="muted small block">{detail}</span>}
                        {notInSpace && (
                          <span className="small block">
                            Not in this space: can't open it until they join as a guest
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
                      {canShare && !g.inherited_from && grantable.includes(g.role) ? (
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
            <label>
              People who can edit can also share
              <select
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
                  )
                }
              >
                <option value="inherit">Same as the folder above (now: {(node.editors_can_share_effective ?? true) ? 'yes' : 'no'})</option>
                <option value="true">Yes</option>
                <option value="false">No</option>
              </select>
            </label>
          </section>
        )}
      </div>
    </Dialog>
  )
}

function AddPersonForm(props: {
  onAdd: (body: Schemas['AddGrantRequest'], inviteGuest: boolean) => Promise<void>
  roles: Schemas['Role'][]
  spaceName: string
  outside: (email: string) => boolean | null
  canInvite: boolean
}) {
  const { roles, spaceName } = props
  const [email, setEmail] = useState('')
  const [role, setRole] = useState<Schemas['Role']>('view')
  const [invite, setInvite] = useState(true)
  const [busy, setBusy] = useState(false)
  const valid = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim())
  const isOutside = valid ? props.outside(email) : null

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    try {
      // The server sends no email for a share yet (crates/ruum-api/src/routes/sharing.rs), so the UI
      // does not offer it: the person must not think the other one was told.
      await props.onAdd({ grantee: { type: 'email', email: email.trim() }, role, notify: false }, isOutside === true && props.canInvite && invite)
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
        <button type="submit" className="primary" disabled={busy}>
          Share
        </button>
      </div>
      <p id="add-role-meaning" className="muted small">
        {roleLabels[role]}: {roleMeanings[role]}
      </p>
      {isOutside === true &&
        (props.canInvite ? (
          <div className="stack">
            <label className="check">
              <input type="checkbox" checked={invite} onChange={(e) => setInvite(e.target.checked)} aria-describedby="add-outside" />
              Also invite them to {spaceName} as a guest
            </label>
            <p id="add-outside" className="muted small">
              {email.trim()} is not in {spaceName}. They can open the item only after they join it as a guest. Ruum emails them the
              invitation.
            </p>
          </div>
        ) : (
          <p className="warning" role="note">
            {email.trim()} is not in {spaceName}. They can open the item only after they join it as a guest. Ask an admin of {spaceName} to
            invite them.
          </p>
        ))}
      <label className="check muted">
        <input type="checkbox" checked={false} disabled aria-describedby="no-share-mail" onChange={() => {}} />
        Send an email
      </label>
      <p id="no-share-mail" className="muted small">
        Ruum can't send emails for shares yet. Tell the person yourself. People in this space see the item under Shared with me; a
        person outside it must join the space as a guest first.
      </p>
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
          <select value={role} onChange={(e) => setRole(e.target.value as Schemas['ShareLinkRole'])}>
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
