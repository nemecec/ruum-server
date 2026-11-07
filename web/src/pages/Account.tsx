import { useState, type FormEvent } from 'react'
import { client, unwrap } from '../api/client'
import { useApi } from '../api/useApi'
import { useSession } from '../app/useSession'
import { StartTeamDialog } from '../app/Shell'
import { useTenant } from '../app/tenant'
import { formatDate } from '../components/format'
import {
  Actions,
  Badge,
  Button,
  Card,
  Code,
  ConfirmButton,
  Dialog,
  ErrorNote,
  Fact,
  Facts,
  Field,
  List,
  ListEmpty,
  ListRow,
  ListRowMain,
  Loaded,
  Notice,
  PageBody,
  SecretBox,
  Stack,
  Text,
  TextInput,
  Toolbar,
} from '../ui'

const providerNames: Record<string, string> = { google: 'Google', microsoft: 'Microsoft', passkey: 'Passkey' }

/** The account page: profile, sign-in methods, sessions, app passwords and new team tenants. */
export function AccountPage() {
  const { me } = useSession()
  const identities = useApi(() => unwrap(client.GET('/v1/me/identities')), [])
  const sessions = useApi(() => unwrap(client.GET('/v1/me/sessions')), [])
  const [notice, setNotice] = useState<string | null>(null)

  return (
    <PageBody>
      <h1>Account</h1>
      <Notice text={notice} />
      <Card aria-labelledby="profile-title">
        <h2 id="profile-title">Profile</h2>
        <Facts>
          <Fact label="Name">{me.user.display_name}</Fact>
          <Fact label="Email">
            {me.user.email} {me.user.email_verified ? <Badge tone="success">Verified</Badge> : <Badge tone="warning">Not verified</Badge>}
          </Fact>
          <Fact label="Member since">{formatDate(me.user.created_at)}</Fact>
        </Facts>
      </Card>

      <Card aria-labelledby="identities-title">
        <h2 id="identities-title">Sign-in methods</h2>
        <Loaded state={identities}>
          {({ items }) => (
            <List>
              {items.map((i) => (
                <ListRow key={i.id}>
                  <ListRowMain>
                    {providerNames[i.provider ?? ''] ?? i.provider ?? i.issuer}
                    {i.email && (
                      <Text as="span" tone="muted">
                        {' '}
                        - {i.email}
                      </Text>
                    )}
                  </ListRowMain>
                  <Text as="span" tone="muted" size="sm">
                    Linked {formatDate(i.linked_at)}
                  </Text>
                </ListRow>
              ))}
            </List>
          )}
        </Loaded>
        <Text tone="muted" size="sm">
          Passkeys and linked accounts are managed by the identity provider at sign-in.
        </Text>
      </Card>

      <Card aria-labelledby="sessions-title">
        <h2 id="sessions-title">Signed-in devices</h2>
        <Loaded state={sessions}>
          {({ items }) => (
            <List>
              {items.map((s) => (
                <ListRow key={s.id}>
                  <ListRowMain>
                    {s.user_agent ?? 'Unknown device'} {s.current && <Badge tone="primary">This device</Badge>}
                    <Text as="span" tone="muted" size="sm">
                      {' '}
                      - last seen {formatDate(s.last_seen_at)}
                    </Text>
                  </ListRowMain>
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
                </ListRow>
              ))}
            </List>
          )}
        </Loaded>
      </Card>

      <AppPasswords />
      <NewTeam />
    </PageBody>
  )
}

function AppPasswords() {
  const { tenant } = useTenant()
  const passwords = useApi(() => unwrap(client.GET('/v1/me/app-passwords')), [])
  const [creating, setCreating] = useState(false)
  const [name, setName] = useState('')
  const [secret, setSecret] = useState<string | null>(null)
  const [davUrl, setDavUrl] = useState<string | null>(null)
  const [error, setError] = useState<unknown>()

  async function create(e: FormEvent) {
    e.preventDefault()
    setError(undefined)
    try {
      const res = await unwrap(client.POST('/v1/me/app-passwords', { body: { name: name.trim() } }))
      setSecret(res.secret)
      setDavUrl(res.dav_url ?? null)
      setName('')
      passwords.reload()
    } catch (err) {
      setError(err)
    }
  }

  return (
    <Card aria-labelledby="apppw-title">
      <Toolbar>
        <h2 id="apppw-title">App passwords</h2>
        <Button onClick={() => (setSecret(null), setCreating(true))}>New app password</Button>
      </Toolbar>
      <Text tone="muted" size="sm">
        For WebDAV clients (Finder, Explorer) in {tenant.name}. Sign in with your email and the app password.
      </Text>
      <Loaded state={passwords}>
        {({ items }) => (
          <List>
            {items.length === 0 && <ListEmpty>No app passwords.</ListEmpty>}
            {items.map((p) => (
              <ListRow key={p.id}>
                <ListRowMain>
                  {p.name} <Code>{p.prefix}...</Code>
                  <Text as="span" tone="muted" size="sm">
                    {' '}
                    - last used {p.last_used_at ? formatDate(p.last_used_at) : 'never'}
                  </Text>
                </ListRowMain>
                <ConfirmButton
                  label="Turn off"
                  ariaLabel={`Turn off app password ${p.name}`}
                  title="Turn off this app password?"
                  message="Programs that use it stop working at once and must sign in again."
                  confirmLabel="Turn off app password"
                  danger
                  onConfirm={async () => {
                    await unwrap(client.DELETE('/v1/me/app-passwords/{app_password_id}', { params: { path: { app_password_id: p.id } } }))
                    passwords.reload()
                  }}
                />
              </ListRow>
            ))}
          </List>
        )}
      </Loaded>
      {creating && (
        <Dialog title="New app password" open onClose={() => setCreating(false)}>
          {secret ? (
            <Stack gap={4}>
              <SecretBox label="App password" secret={secret} />
              {davUrl && <SecretBox label="Server address" secret={davUrl} />}
              <Text tone="muted" size="sm">
                Connect your WebDAV client to the server address, with your email as the user name. The password is shown only once.
              </Text>
              <Actions>
                <Button variant="primary" onClick={() => setCreating(false)}>
                  Done
                </Button>
              </Actions>
            </Stack>
          ) : (
            <Stack as="form" gap={4} onSubmit={create}>
              <Field label="Name">
                <TextInput required autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Finder on my laptop" />
              </Field>
              <ErrorNote error={error} />
              <Actions>
                <Button onClick={() => setCreating(false)}>Cancel</Button>
                <Button type="submit" variant="primary" disabled={!name.trim()}>
                  Create
                </Button>
              </Actions>
            </Stack>
          )}
        </Dialog>
      )}
    </Card>
  )
}

function NewTeam() {
  const [open, setOpen] = useState(false)
  return (
    <Card aria-labelledby="team-title">
      <h2 id="team-title">Team spaces</h2>
      <Text tone="muted">A team space is shared by the people you invite. You'll be its owner.</Text>
      <Actions align="start">
        <Button onClick={() => setOpen(true)}>Start a team space</Button>
      </Actions>
      {open && <StartTeamDialog onClose={() => setOpen(false)} />}
    </Card>
  )
}
