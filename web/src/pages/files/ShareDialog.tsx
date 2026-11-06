import { useState, type FormEvent } from 'react'
import { client, unwrap, type Schemas } from '../../api/client'
import { usePagedApi } from '../../api/usePagedApi'
import { useTenant } from '../../app/tenant'
import { formatDate, roleLabels } from '../../components/format'
import { ConfirmButton, Dialog, ErrorNote, Loaded, LoadMore, Notice, SecretBox } from '../../components/ui'
import type { Node } from './api'

const roles: Schemas['Role'][] = ['view', 'comment', 'edit', 'manage']
const linkRoles: Schemas['ShareLinkRole'][] = ['view', 'comment', 'edit']

/** The roles a caller with `mine` may grant: at most its own role (decision 0001-1). */
function grantableRoles(mine: Schemas['Role'] | null | undefined): Schemas['Role'][] {
  const top = mine ? roles.indexOf(mine) : -1
  return roles.filter((_, i) => i <= top)
}

function granteeName(g: Schemas['Grantee']): string {
  switch (g.type) {
    case 'user':
      return g.display_name ?? g.email ?? g.id
    case 'email':
      return g.email
    case 'tenant_members':
      return 'All members of the tenant'
  }
}

/** The share dialog: people with access, share links, and who may share further. */
export function ShareDialog(props: { node: Node; open: boolean; onClose: () => void; onChanged: () => void }) {
  const { node } = props
  const { tenant } = useTenant()
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

  const canShare = node.my_can_share ?? node.my_role === 'manage'
  const canManage = node.my_role === 'manage'
  const grantable = grantableRoles(node.my_role)
  // Share links never give manage, and never more than the caller has.
  const linkGrantable = linkRoles.filter((r) => grantable.includes(r))

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
            onAdd={(body) =>
              run(
                () => unwrap(client.POST('/v1/nodes/{node_id}/grants', { params: { path: { node_id: node.id } }, body })),
                `Shared with ${granteeName(body.grantee)}.`,
                grants.reload,
              )
            }
          />
        ) : (
          <p className="muted">You can see who has access, but you cannot share this item.</p>
        )}

        <section aria-labelledby="share-people">
          <h3 id="share-people">People with access</h3>
          <Loaded state={grants}>
            {(page) => (
              <ul className="rows">
                {page.items.length === 0 && <li className="muted">Only you.</li>}
                {page.items.map((g) => (
                  <li key={g.id} className="row">
                    <span className="grow">
                      {granteeName(g.grantee)}
                      {g.inherited_from && <span className="muted small"> (from a parent folder)</span>}
                    </span>
                    {canShare && !g.inherited_from && grantable.includes(g.role) ? (
                      <>
                        <label className="sr-only" htmlFor={`role-${g.id}`}>
                          Role of {granteeName(g.grantee)}
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
                              `${granteeName(g.grantee)} ${roleLabels[e.target.value as Schemas['Role']].toLowerCase()}.`,
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
                          aria-label={`Remove access of ${granteeName(g.grantee)}`}
                          onClick={() =>
                            run(
                              () =>
                                unwrap(
                                  client.DELETE('/v1/nodes/{node_id}/grants/{grant_id}', {
                                    params: { path: { node_id: node.id, grant_id: g.id } },
                                  }),
                                ),
                              `Removed ${granteeName(g.grantee)}.`,
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
                ))}
              </ul>
            )}
          </Loaded>
          <LoadMore state={grants} />
        </section>

        <section aria-labelledby="share-links">
          <h3 id="share-links">Share links</h3>
          {!policy.allow_share_links ? (
            <p className="muted">Share links are off in this tenant.</p>
          ) : (
            <>
              <Loaded state={links}>
                {(page) => {
                  // The list has revoked links too; only live ones are shown.
                  const live = page.items.filter((l) => !l.revoked_at)
                  return (
                    <ul className="rows">
                      {live.length === 0 && <li className="muted">No links.</li>}
                      {live.map((l) => (
                        <li key={l.id} className="row">
                          <span className="grow">
                            {roleLabels[l.role]} link{l.has_password ? ', with password' : ''}
                            <span className="muted small"> - expires {l.expires_at ? formatDate(l.expires_at) : 'never'}</span>
                          </span>
                          {canShare && (
                            <ConfirmButton
                              label="Revoke"
                              ariaLabel={`Revoke ${roleLabels[l.role].toLowerCase()} link`}
                              title="Revoke the link?"
                              message="People with this link lose access at once."
                              confirmLabel="Revoke link"
                              danger
                              onConfirm={() =>
                                run(
                                  () => unwrap(client.DELETE('/v1/share-links/{link_id}', { params: { path: { link_id: l.id } } })),
                                  'Link revoked.',
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
            <h3 id="share-settings">Settings</h3>
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
                    'Sharing settings saved.',
                    props.onChanged,
                  )
                }
              >
                <option value="inherit">
                  Inherit from the parent folder ({(node.editors_can_share_effective ?? true) ? 'yes' : 'no'} now)
                </option>
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

function AddPersonForm({ onAdd, roles }: { onAdd: (body: Schemas['AddGrantRequest']) => Promise<void>; roles: Schemas['Role'][] }) {
  const [email, setEmail] = useState('')
  const [role, setRole] = useState<Schemas['Role']>('view')
  const [notify, setNotify] = useState(true)

  async function submit(e: FormEvent) {
    e.preventDefault()
    await onAdd({ grantee: { type: 'email', email: email.trim() }, role, notify })
    setEmail('')
  }

  return (
    <form className="inline-form" onSubmit={submit} aria-label="Add people">
      <label className="grow">
        Email
        <input type="email" required value={email} onChange={(e) => setEmail(e.target.value)} placeholder="name@example.com" />
      </label>
      <label>
        Role
        <select value={role} onChange={(e) => setRole(e.target.value as Schemas['Role'])}>
          {roles.map((r) => (
            <option key={r} value={r}>
              {roleLabels[r]}
            </option>
          ))}
        </select>
      </label>
      <label className="check">
        <input type="checkbox" checked={notify} onChange={(e) => setNotify(e.target.checked)} />
        Send an email
      </label>
      <button type="submit" className="primary">
        Share
      </button>
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
  const [days, setDays] = useState<string>(props.maxDays ? String(Math.min(7, props.maxDays)) : '')
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
    <form className="stack" onSubmit={submit} aria-label="Create a share link">
      <div className="inline-form">
        <label>
          Link role
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
          Expires in days
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
      <ErrorNote error={error} />
      {created?.url && <SecretBox label="New link" secret={created.url} />}
    </form>
  )
}
