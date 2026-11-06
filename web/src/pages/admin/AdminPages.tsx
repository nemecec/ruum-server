import { useState, type FormEvent } from 'react'
import { NavLink, Outlet } from 'react-router'
import { client, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { useSession } from '../../app/useSession'
import { useTenant } from '../../app/tenant'
import { actorText, formatBytes, formatDate, memberRoleLabels } from '../../components/format'
import { ConfirmButton, ErrorNote, Loaded, LoadMore, Notice } from '../../components/ui'

/** The tenant admin layout: a sub-navigation and one admin page. */
export function AdminLayout() {
  const { tenant, isAdmin } = useTenant()
  if (!isAdmin) {
    return (
      <div className="page-body">
        <h1>Admin</h1>
        <p>Only admins and owners of {tenant.name} can open these pages.</p>
      </div>
    )
  }
  const tabs: [string, string][] = [
    ['members', 'Members'],
    ['invitations', 'Invitations'],
    ['policy', 'Policy'],
    ['limits', 'Plan and limits'],
    ['audit', 'Audit log'],
  ]
  return (
    <div className="page-body">
      <h1>Admin: {tenant.name}</h1>
      <nav aria-label="Admin" className="tabs">
        <ul>
          {tabs.map(([to, label]) => (
            <li key={to}>
              <NavLink to={to}>{label}</NavLink>
            </li>
          ))}
        </ul>
      </nav>
      <Outlet />
    </div>
  )
}

const assignable: Schemas['MemberRole'][] = ['guest', 'member', 'admin', 'owner']

export function MembersPage() {
  const { tenant, role: myRole } = useTenant()
  const { me } = useSession()
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const members = usePagedApi(
    (cursor) => unwrap(client.GET('/v1/tenants/{tenant_id}/members', { params: { path: { tenant_id: tenant.id }, query: cursor ? { cursor } : {} } })),
    [tenant.id],
  )

  async function changeRole(m: Schemas['Member'], role: Schemas['MemberRole']) {
    setError(undefined)
    try {
      await unwrap(
        client.PATCH('/v1/tenants/{tenant_id}/members/{user_id}', {
          params: { path: { tenant_id: tenant.id, user_id: m.user_id } },
          body: { role },
        }),
      )
      setNotice(`${m.display_name} is now ${memberRoleLabels[role].toLowerCase()}.`)
    } catch (e) {
      setError(e)
    }
    members.reload()
  }

  return (
    <section aria-labelledby="members-title">
      <h2 id="members-title">Members</h2>
      <ErrorNote error={error} />
      <Notice text={notice} />
      <Loaded state={members}>
        {({ items }) => (
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Name</th>
                <th scope="col" className="hide-narrow">
                  Email
                </th>
                <th scope="col">Role</th>
                <th scope="col">
                  <span className="sr-only">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {items.map((m) => {
                const self = m.user_id === me.user.id
                // Only owners can make or change owners.
                const locked = self || (m.role === 'owner' && myRole !== 'owner')
                return (
                  <tr key={m.user_id}>
                    <th scope="row">
                      {m.display_name}
                      {self && <span className="muted"> (you)</span>}
                    </th>
                    <td className="hide-narrow">{m.email}</td>
                    <td>
                      <label className="sr-only" htmlFor={`member-role-${m.user_id}`}>
                        Role of {m.display_name}
                      </label>
                      <select
                        id={`member-role-${m.user_id}`}
                        value={m.role}
                        disabled={locked}
                        onChange={(e) => changeRole(m, e.target.value as Schemas['MemberRole'])}
                      >
                        {assignable
                          .filter((r) => r !== 'owner' || myRole === 'owner' || m.role === 'owner')
                          .map((r) => (
                            <option key={r} value={r}>
                              {memberRoleLabels[r]}
                            </option>
                          ))}
                      </select>
                    </td>
                    <td className="row-actions">
                      {!locked && (
                        <ConfirmButton
                          label="Remove"
                          ariaLabel={`Remove ${m.display_name}`}
                          title={`Remove ${m.display_name}?`}
                          message="They lose access to the tenant. Their agents in this tenant are disabled."
                          confirmLabel="Remove member"
                          danger
                          onConfirm={async () => {
                            await unwrap(
                              client.DELETE('/v1/tenants/{tenant_id}/members/{user_id}', {
                                params: { path: { tenant_id: tenant.id, user_id: m.user_id } },
                              }),
                            )
                            setNotice(`${m.display_name} removed.`)
                            members.reload()
                          }}
                        />
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        )}
      </Loaded>
      <LoadMore state={members} />
    </section>
  )
}

export function InvitationsPage() {
  const { tenant } = useTenant()
  const [email, setEmail] = useState('')
  const [role, setRole] = useState<Schemas['CreateInvitationRequest']['role']>('member')
  const [message, setMessage] = useState('')
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()
  const invitations = usePagedApi(
    (cursor) => unwrap(client.GET('/v1/tenants/{tenant_id}/invitations', { params: { path: { tenant_id: tenant.id }, query: cursor ? { cursor } : {} } })),
    [tenant.id],
  )

  async function invite(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      await unwrap(
        client.POST('/v1/tenants/{tenant_id}/invitations', {
          params: { path: { tenant_id: tenant.id } },
          body: { email: email.trim(), role, ...(message.trim() ? { message: message.trim() } : {}) },
        }),
      )
      setNotice(`Invitation sent to ${email.trim()}.`)
      setEmail('')
      setMessage('')
      invitations.reload()
    } catch (err) {
      setError(err)
    }
  }

  return (
    <section aria-labelledby="inv-title">
      <h2 id="inv-title">Invitations</h2>
      <form className="stack card" onSubmit={invite} aria-label="Invite someone">
        <div className="inline-form">
          <label className="grow">
            Email
            <input type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
          </label>
          <label>
            Role
            <select value={role} onChange={(e) => setRole(e.target.value as typeof role)}>
              <option value="guest">Guest</option>
              <option value="member">Member</option>
              <option value="admin">Admin</option>
            </select>
          </label>
        </div>
        <label>
          Message (optional)
          <textarea rows={2} value={message} onChange={(e) => setMessage(e.target.value)} />
        </label>
        {!tenant.policy.allow_guests && role === 'guest' && <p className="error">Guests are off in the policy of this tenant.</p>}
        <ErrorNote error={error} />
        <div className="actions">
          <button type="submit" className="primary">
            Send invitation
          </button>
        </div>
      </form>
      <Notice text={notice} />
      <Loaded state={invitations}>
        {({ items }) => (
          <ul className="rows">
            {items.length === 0 && <li className="muted">No open invitations.</li>}
            {items.map((i) => (
              <li key={i.id} className="row">
                <span className="grow">
                  {i.email} <span className="badge">{memberRoleLabels[i.role]}</span>
                  <span className="muted small"> - expires {formatDate(i.expires_at)}</span>
                </span>
                <ConfirmButton
                  label="Revoke"
                  ariaLabel={`Revoke invitation of ${i.email}`}
                  title="Revoke this invitation?"
                  message={`${i.email} can no longer join with it.`}
                  confirmLabel="Revoke"
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
              </li>
            ))}
          </ul>
        )}
      </Loaded>
      <LoadMore state={invitations} />
    </section>
  )
}

export function PolicyPage() {
  const { tenant } = useTenant()
  const { reloadTenants } = useSession()
  const [name, setName] = useState(tenant.name)
  const [policy, setPolicy] = useState<Schemas['TenantPolicy']>(tenant.policy)
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()

  async function save(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      await unwrap(client.PATCH('/v1/tenants/{tenant_id}', { params: { path: { tenant_id: tenant.id } }, body: { name: name.trim(), policy } }))
      setNotice('Policy saved.')
      reloadTenants()
    } catch (err) {
      setError(err)
    }
  }

  const check = (key: keyof Omit<Schemas['TenantPolicy'], 'share_link_max_days'>, label: string, disabled = false) => (
    <label className="check">
      <input type="checkbox" checked={policy[key]} disabled={disabled} onChange={(e) => setPolicy({ ...policy, [key]: e.target.checked })} />
      {label}
    </label>
  )

  return (
    <section aria-labelledby="policy-title">
      <h2 id="policy-title">Policy</h2>
      <form className="stack card" onSubmit={save}>
        <label>
          Tenant name
          <input required value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <fieldset className="stack">
          <legend>Sharing</legend>
          {check('members_can_share', 'Members can share with others')}
          {check('allow_guests', 'Allow guests (people outside the tenant)')}
          {check('allow_share_links', 'Allow share links')}
          {check('allow_edit_links', 'Allow share links that can edit', !policy.allow_share_links)}
          <label>
            Share links expire after at most (days, empty for no limit)
            <input
              type="number"
              min={1}
              value={policy.share_link_max_days ?? ''}
              disabled={!policy.allow_share_links}
              onChange={(e) => setPolicy({ ...policy, share_link_max_days: e.target.value ? Number(e.target.value) : null })}
            />
          </label>
        </fieldset>
        <ErrorNote error={error} />
        <Notice text={notice} />
        <div className="actions">
          <button type="submit" className="primary">
            Save policy
          </button>
        </div>
      </form>
    </section>
  )
}

function Meter({ label, used, limit, format = String }: { label: string; used: number; limit: number | null; format?: (n: number) => string }) {
  return (
    <div className="meter-row">
      <div className="row">
        <span className="grow">{label}</span>
        <span>
          {format(used)} of {limit === null ? 'no limit' : format(limit)}
        </span>
      </div>
      {limit !== null && <meter min={0} max={limit} value={used} high={limit * 0.9} aria-label={label} />}
    </div>
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
      <h2 id="limits-title">Plan and limits</h2>
      <Loaded state={limits}>
        {({ plan, limits: l, usage: u }) => (
          <div className="stack card">
            <p>
              Plan: <strong>{plan[0].toUpperCase() + plan.slice(1)}</strong>
            </p>
            <Meter label="Storage" used={u.storage_bytes} limit={l.storage_bytes} format={formatBytes} />
            <Meter label="Agents" used={u.agents} limit={l.agents} />
            <Meter label="Members" used={u.members} limit={l.members} />
            <dl className="facts">
              <dt>Largest file</dt>
              <dd>{formatBytes(l.max_file_bytes)}</dd>
              <dt>Versions kept</dt>
              <dd>
                {l.version_days === null ? 'Forever' : `${l.version_days} days`}
                {l.version_count !== null && `, at most ${l.version_count} per file`}
              </dd>
              <dt>Trash kept</dt>
              <dd>{l.trash_days} days</dd>
              <dt>Admin restore</dt>
              <dd>{l.admin_restore_days ? `${l.admin_restore_days} days` : 'Not in this plan'}</dd>
              <dt>Nodes</dt>
              <dd>{u.nodes}</dd>
            </dl>
          </div>
        )}
      </Loaded>
    </section>
  )
}

export function AuditPage() {
  const { tenant } = useTenant()
  const [action, setAction] = useState('')
  const [filter, setFilter] = useState('')
  // The audit log also comes as NDJSON (for export); the UI asks for JSON pages.
  const audit = usePagedApi(
    async (cursor) =>
      (await unwrap(
        client.GET('/v1/tenants/{tenant_id}/audit', {
          params: { path: { tenant_id: tenant.id }, query: { ...(filter ? { action: filter } : {}), ...(cursor ? { cursor } : {}) } },
        }),
      )) as Schemas['AuditPage'],
    [tenant.id, filter],
  )
  return (
    <section aria-labelledby="audit-title">
      <h2 id="audit-title">Audit log</h2>
      <form
        className="inline-form"
        onSubmit={(e) => {
          e.preventDefault()
          setFilter(action.trim())
        }}
      >
        <label className="grow">
          Action starts with
          <input value={action} onChange={(e) => setAction(e.target.value)} placeholder="member." />
        </label>
        <button type="submit">Filter</button>
      </form>
      <Loaded state={audit}>
        {({ items }) => (
          <>
            {items.length === 0 ? (
              <p className="empty">No entries.</p>
            ) : (
              <table className="table">
                <thead>
                  <tr>
                    <th scope="col">When</th>
                    <th scope="col">Action</th>
                    <th scope="col">Who</th>
                    <th scope="col" className="hide-narrow">
                      Target
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((a) => (
                    <tr key={a.id}>
                      <td>{formatDate(a.at)}</td>
                      <th scope="row">
                        <code>{a.action}</code>
                      </th>
                      <td>{actorText(a.actor, a.on_behalf_of)}</td>
                      <td className="hide-narrow">{a.target_type ? `${a.target_type} ${a.target_id ?? ''}` : '-'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </>
        )}
      </Loaded>
      <LoadMore state={audit} />
    </section>
  )
}
