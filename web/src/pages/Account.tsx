import { useState, type FormEvent } from 'react'
import { useNavigate } from 'react-router'
import { client, unwrap } from '../api/client'
import { useApi } from '../api/useApi'
import { useSession } from '../app/useSession'
import { tenantPath, useTenant } from '../app/tenant'
import { formatDate } from '../components/format'
import { ConfirmButton, Dialog, ErrorNote, Loaded, Notice, SecretBox } from '../components/ui'

const providerNames: Record<string, string> = { google: 'Google', microsoft: 'Microsoft', passkey: 'Passkey' }

/** The account page: profile, sign-in methods, sessions, app passwords and new team tenants. */
export function AccountPage() {
  const { me } = useSession()
  const identities = useApi(() => unwrap(client.GET('/v1/me/identities')), [])
  const sessions = useApi(() => unwrap(client.GET('/v1/me/sessions')), [])
  const [notice, setNotice] = useState<string | null>(null)

  return (
    <div className="page-body">
      <h1>Account</h1>
      <Notice text={notice} />
      <section className="card" aria-labelledby="profile-title">
        <h2 id="profile-title">Profile</h2>
        <dl className="facts">
          <dt>Name</dt>
          <dd>{me.user.display_name}</dd>
          <dt>Email</dt>
          <dd>
            {me.user.email} {me.user.email_verified ? <span className="badge">Verified</span> : <span className="badge warn">Not verified</span>}
          </dd>
          <dt>Member since</dt>
          <dd>{formatDate(me.user.created_at)}</dd>
        </dl>
      </section>

      <section className="card" aria-labelledby="identities-title">
        <h2 id="identities-title">Sign-in methods</h2>
        <Loaded state={identities}>
          {({ items }) => (
            <ul className="rows">
              {items.map((i) => (
                <li key={i.id} className="row">
                  <span className="grow">
                    {providerNames[i.provider ?? ''] ?? i.provider ?? i.issuer}
                    {i.email && <span className="muted"> - {i.email}</span>}
                  </span>
                  <span className="muted small">Linked {formatDate(i.linked_at)}</span>
                </li>
              ))}
            </ul>
          )}
        </Loaded>
        <p className="muted small">Passkeys and linked accounts are managed by the identity provider at sign-in.</p>
      </section>

      <section className="card" aria-labelledby="sessions-title">
        <h2 id="sessions-title">Sessions</h2>
        <Loaded state={sessions}>
          {({ items }) => (
            <ul className="rows">
              {items.map((s) => (
                <li key={s.id} className="row">
                  <span className="grow">
                    {s.user_agent ?? 'Unknown device'} {s.current && <span className="badge">This device</span>}
                    <span className="muted small"> - last seen {formatDate(s.last_seen_at)}</span>
                  </span>
                  {!s.current && (
                    <ConfirmButton
                      label="Sign out"
                      ariaLabel={`Sign out ${s.user_agent ?? 'session'}`}
                      title="End this session?"
                      message="The device must sign in again."
                      confirmLabel="End session"
                      onConfirm={async () => {
                        await unwrap(client.DELETE('/v1/me/sessions/{session_id}', { params: { path: { session_id: s.id } } }))
                        setNotice('Session ended.')
                        sessions.reload()
                      }}
                    />
                  )}
                </li>
              ))}
            </ul>
          )}
        </Loaded>
      </section>

      <AppPasswords />
      <NewTeam />
    </div>
  )
}

function AppPasswords() {
  const { tenant } = useTenant()
  const passwords = useApi(() => unwrap(client.GET('/v1/me/app-passwords')), [])
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')
  const [secret, setSecret] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()

  async function create(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await unwrap(client.POST('/v1/me/app-passwords', { body: { name: name.trim() } }))
      setSecret(res.secret)
      setName('')
      passwords.reload()
    } catch (err) {
      setError(err)
    }
  }

  return (
    <section className="card" aria-labelledby="apppw-title">
      <div className="panel-head">
        <h2 id="apppw-title">App passwords</h2>
        <button type="button" onClick={() => (setSecret(null), setCreating(true))}>
          New app password
        </button>
      </div>
      <p className="muted small">For WebDAV clients (Finder, Explorer) in {tenant.name}.</p>
      <Loaded state={passwords}>
        {({ items }) => (
          <ul className="rows">
            {items.length === 0 && <li className="muted">No app passwords.</li>}
            {items.map((p) => (
              <li key={p.id} className="row">
                <span className="grow">
                  {p.name} <code>{p.prefix}...</code>
                  <span className="muted small"> - last used {p.last_used_at ? formatDate(p.last_used_at) : 'never'}</span>
                </span>
                <ConfirmButton
                  label="Revoke"
                  ariaLabel={`Revoke app password ${p.name}`}
                  title="Revoke this app password?"
                  message="Clients that use it must sign in again."
                  confirmLabel="Revoke"
                  danger
                  onConfirm={async () => {
                    await unwrap(client.DELETE('/v1/me/app-passwords/{app_password_id}', { params: { path: { app_password_id: p.id } } }))
                    passwords.reload()
                  }}
                />
              </li>
            ))}
          </ul>
        )}
      </Loaded>
      {creating && (
        <Dialog title="New app password" open onClose={() => setCreating(false)}>
          {secret ? (
            <div className="stack">
              <SecretBox label="App password" secret={secret} />
              <div className="actions">
                <button type="button" className="primary" onClick={() => setCreating(false)}>
                  Done
                </button>
              </div>
            </div>
          ) : (
            <form className="stack" onSubmit={create}>
              <label>
                Name
                <input required autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Finder on my laptop" />
              </label>
              <ErrorNote error={error} />
              <div className="actions">
                <button type="button" onClick={() => setCreating(false)}>
                  Cancel
                </button>
                <button type="submit" className="primary" disabled={!name.trim()}>
                  Create
                </button>
              </div>
            </form>
          )}
        </Dialog>
      )}
    </section>
  )
}

function NewTeam() {
  const { reloadTenants } = useSession()
  const navigate = useNavigate()
  const [name, setName] = useState('')
  const [error, setError] = useState<unknown>()

  async function submit(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      const t = await unwrap(client.POST('/v1/tenants', { body: { name: name.trim() } }))
      reloadTenants()
      navigate(tenantPath(t.id, 'admin'))
    } catch (err) {
      setError(err)
    }
  }

  return (
    <section className="card" aria-labelledby="team-title">
      <h2 id="team-title">Start a team</h2>
      <form className="inline-form" onSubmit={submit}>
        <label className="grow">
          Team name
          <input required value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <button type="submit" disabled={!name.trim()}>
          Create team
        </button>
      </form>
      <ErrorNote error={error} />
    </section>
  )
}
