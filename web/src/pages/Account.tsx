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
  Select,
  SecretBox,
  Stack,
  Text,
  TextInput,
  Toolbar,
} from '../ui'
import { browserLanguages, catalogueFor } from '../i18n/config'
import { LANGUAGES, PSEUDO_LOCALE, t, useLocale, type MessageKey } from '../i18n'

const providerNames: Record<string, MessageKey> = { google: 'account.provider.google', microsoft: 'account.provider.microsoft', passkey: 'account.provider.passkey' }

/** The account page: profile, sign-in methods, sessions, app passwords and new team tenants. */
export function AccountPage() {
  const { me } = useSession()
  const identities = useApi(() => unwrap(client.GET('/v1/me/identities')), [])
  const sessions = useApi(() => unwrap(client.GET('/v1/me/sessions')), [])
  const [notice, setNotice] = useState<string | null>(null)

  return (
    <PageBody>
      <h1>{t('account.title')}</h1>
      <Notice text={notice} />
      <Card aria-labelledby="profile-title">
        <h2 id="profile-title">{t('account.profile')}</h2>
        <Facts>
          <Fact label={t('account.name')}>{me.user.display_name}</Fact>
          <Fact label={t('account.email')}>
            {me.user.email}{' '}
            {me.user.email_verified ? (
              <Badge tone="success">{t('account.verified')}</Badge>
            ) : (
              <Badge tone="warning">{t('account.notVerified')}</Badge>
            )}
          </Fact>
          <Fact label={t('account.memberSince')}>{formatDate(me.user.created_at)}</Fact>
        </Facts>
      </Card>

      <LanguageCard />

      <Card aria-labelledby="identities-title">
        <h2 id="identities-title">{t('account.identities')}</h2>
        <Loaded state={identities}>
          {({ items }) => (
            <List>
              {items.map((i) => (
                <ListRow key={i.id}>
                  <ListRowMain>
                    {providerNames[i.provider ?? ''] ? t(providerNames[i.provider ?? '']) : (i.provider ?? i.issuer)}
                    {i.email && (
                      <Text as="span" tone="muted">
                        {' '}
                        {t('account.identity.email', { email: i.email })}
                      </Text>
                    )}
                  </ListRowMain>
                  <Text as="span" tone="muted" size="sm">
                    {t('account.identity.linked', { when: formatDate(i.linked_at) })}
                  </Text>
                </ListRow>
              ))}
            </List>
          )}
        </Loaded>
        <Text tone="muted" size="sm">
          {t('account.identities.note')}
        </Text>
      </Card>

      <Card aria-labelledby="sessions-title">
        <h2 id="sessions-title">{t('account.sessions')}</h2>
        <Loaded state={sessions}>
          {({ items }) => (
            <List>
              {items.map((s) => (
                <ListRow key={s.id}>
                  <ListRowMain>
                    {s.user_agent ?? t('account.session.unknown')} {s.current && <Badge tone="primary">{t('account.session.current')}</Badge>}
                    <Text as="span" tone="muted" size="sm">
                      {' '}
                      {t('account.session.lastSeen', { when: formatDate(s.last_seen_at) })}
                    </Text>
                  </ListRowMain>
                  {!s.current && (
                    <ConfirmButton
                      label={t('account.session.signOut')}
                      ariaLabel={s.user_agent ? t('account.session.signOut.named', { name: s.user_agent }) : t('account.session.signOut.session')}
                      title={t('account.session.end.title')}
                      message={t('account.session.end.message')}
                      confirmLabel={t('account.session.end.confirm')}
                      onConfirm={async () => {
                        await unwrap(client.DELETE('/v1/me/sessions/{session_id}', { params: { path: { session_id: s.id } } }))
                        setNotice(t('account.session.ended'))
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
        <h2 id="apppw-title">{t('account.appPasswords')}</h2>
        <Button onClick={() => (setSecret(null), setCreating(true))}>{t('account.appPasswords.new')}</Button>
      </Toolbar>
      <Text tone="muted" size="sm">
        {t('account.appPasswords.intro', { space: tenant.name })}
      </Text>
      <Loaded state={passwords}>
        {({ items }) => (
          <List>
            {items.length === 0 && <ListEmpty>{t('account.appPasswords.empty')}</ListEmpty>}
            {items.map((p) => (
              <ListRow key={p.id}>
                <ListRowMain>
                  {p.name} <Code>{p.prefix}...</Code>
                  <Text as="span" tone="muted" size="sm">
                    {' '}
                    {p.last_used_at ? t('account.appPassword.lastUsed', { when: formatDate(p.last_used_at) }) : t('account.appPassword.neverUsed')}
                  </Text>
                </ListRowMain>
                <ConfirmButton
                  label={t('account.appPassword.off')}
                  ariaLabel={t('account.appPassword.off.named', { name: p.name })}
                  title={t('account.appPassword.off.title')}
                  message={t('account.appPassword.off.message')}
                  confirmLabel={t('account.appPassword.off.confirm')}
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
        <Dialog title={t('account.appPasswords.new')} open onClose={() => setCreating(false)}>
          {secret ? (
            <Stack gap={4}>
              <SecretBox label={t('account.appPassword.secret')} secret={secret} />
              {davUrl && <SecretBox label={t('account.appPassword.server')} secret={davUrl} />}
              <Text tone="muted" size="sm">
                {t('account.appPassword.connect')}
              </Text>
              <Actions>
                <Button variant="primary" onClick={() => setCreating(false)}>
                  {t('common.done')}
                </Button>
              </Actions>
            </Stack>
          ) : (
            <Stack as="form" gap={4} onSubmit={create}>
              <Field label={t('account.name')}>
                <TextInput
                  required
                  autoFocus
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder={t('account.appPassword.placeholder')}
                />
              </Field>
              <ErrorNote error={error} />
              <Actions>
                <Button onClick={() => setCreating(false)}>{t('common.cancel')}</Button>
                <Button type="submit" variant="primary" disabled={!name.trim()}>
                  {t('common.create')}
                </Button>
              </Actions>
            </Stack>
          )}
        </Dialog>
      )}
    </Card>
  )
}

/** The value of "Same as the browser" in the language picker. */
const BROWSER = ''

/**
 * The language of the UI (design 12.4): stored with the account (`updateMyPreferences`), so it
 * holds on every device; "Same as the browser" clears it.
 */
function LanguageCard() {
  const { preference, setPreference } = useLocale()
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  const choices = import.meta.env.PROD ? LANGUAGES : [...LANGUAGES, { tag: PSEUDO_LOCALE, name: t('account.language.pseudo') }]
  const browserName = LANGUAGES.find((l) => l.tag === catalogueFor(browserLanguages()[0] ?? ''))?.name ?? LANGUAGES[0].name

  async function change(value: string) {
    setBusy(true)
    setError(undefined)
    try {
      const saved = await unwrap(client.PATCH('/v1/me/preferences', { body: { locale: value === BROWSER ? null : value } }))
      // The page renders again in the new language.
      setPreference(saved.locale)
    } catch (err) {
      setError(err)
      setBusy(false)
    }
  }

  return (
    <Card aria-labelledby="language-title">
      <h2 id="language-title">{t('account.language')}</h2>
      <Field label={t('account.language.label')} hint={t('account.language.hint')}>
        <Select value={preference ?? BROWSER} disabled={busy} onChange={(e) => void change(e.target.value)}>
          <option value={BROWSER}>{t('account.language.browser', { language: browserName })}</option>
          {choices.map((l) => (
            <option key={l.tag} value={l.tag} lang={l.tag === PSEUDO_LOCALE ? undefined : l.tag}>
              {l.name}
            </option>
          ))}
        </Select>
      </Field>
      <ErrorNote error={error} />
    </Card>
  )
}

function NewTeam() {
  const [open, setOpen] = useState(false)
  return (
    <Card aria-labelledby="team-title">
      <h2 id="team-title">{t('account.teams')}</h2>
      <Text tone="muted">{t('account.teams.intro')}</Text>
      <Actions align="start">
        <Button onClick={() => setOpen(true)}>{t('shell.startTeam.title')}</Button>
      </Actions>
      {open && <StartTeamDialog onClose={() => setOpen(false)} />}
    </Card>
  )
}
