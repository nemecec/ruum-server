import { useState, type FormEvent, type ReactNode } from 'react'
import { Navigate, Outlet, useLocation, useNavigate, useParams } from 'react-router'
import { client, RuumApiError, setSigningOut, setTenant, unwrap, type Schemas } from '../api/client'
import { memberRoleLabels } from '../components/format'
import {
  Actions,
  AppBody,
  AppMain,
  Brand,
  Button,
  Dialog,
  ErrorNote,
  Field,
  MenuToggle,
  NavButton,
  NavItem,
  NavList,
  NavRail,
  Note,
  SearchForm,
  Select,
  SkipLink,
  Stack,
  TextInput,
  TopBar,
  TopBarItem,
  useToast,
} from '../ui'
import { browser } from './browser'
import { NamesProvider } from './names'
import { useSession } from './useSession'
import { lastTenant, rememberTenant, TenantContext, tenantPath } from './tenant'
import { t } from '../i18n'

/** `/`: goes to the files of the last space, or of the personal space. */
export function HomeRedirect() {
  const { me, tenants } = useSession()
  const last = lastTenant()
  const id = tenants.some((m) => m.tenant.id === last) ? last! : me.user.home_tenant_id
  return <Navigate to={tenantPath(id, 'files')} replace />
}

/** `/account` without a space goes to the account page of the last or personal space. */
export function AccountRedirect() {
  const { me, tenants } = useSession()
  const last = lastTenant()
  const id = tenants.some((m) => m.tenant.id === last) ? last! : me.user.home_tenant_id
  return <Navigate to={tenantPath(id, 'account')} replace />
}

/**
 * A space that is not in the person's list: someone shared an item of it with them, but Ruum has
 * not listed the space for them (docs/ux/gap-analysis.md, contract need C-7). The pages work with
 * the person's grants; the name of the space is not known.
 */
function sharedSpace(id: string): Schemas['TenantMembership'] {
  return {
    role: 'guest',
    tenant: {
      id,
      name: t('shell.sharedSpace'),
      kind: 'team',
      plan: 'free',
      region: '',
      root_node_id: id,
      created_at: new Date(0).toISOString(),
      policy: {
        members_can_share: false,
        allow_guests: true,
        allow_share_links: false,
        allow_edit_links: false,
        share_link_max_days: null,
      },
    },
  }
}

/** The layout of every page inside a space: the header, the navigation and the page. */
export function TenantLayout() {
  const { tenantId = '' } = useParams()
  const { tenants, me } = useSession()
  const listed = tenants.find((m) => m.tenant.id === tenantId)
  const membership = listed ?? sharedSpace(tenantId)

  // Set the tenant header before the pages below load their data (their effects run first).
  setTenant(tenantId)
  if (listed) rememberTenant(tenantId)
  const isAdmin = membership.role === 'admin' || membership.role === 'owner'

  return (
    <TenantContext.Provider value={{ tenant: membership.tenant, role: membership.role, isAdmin, listed: Boolean(listed) }}>
      <NamesProvider
        tenantId={tenantId}
        canListMembers={membership.role !== 'guest'}
        me={{ id: me.user.id, name: me.user.display_name, email: me.user.email }}
      >
        <SkipLink />
        <Header tenantId={tenantId} isAdmin={isAdmin} unlisted={listed ? null : membership}>
          {/* Only for a guest of the space. A space that is not in the person's list is not theirs at
              all yet: the pages say so (NotInSpaceYet), and a banner that says "guest" would contradict them. */}
          {listed?.role === 'guest' && <Note role="note">{t('shell.guestBanner')}</Note>}
          <Outlet key={tenantId} />
        </Header>
      </NamesProvider>
    </TenantContext.Provider>
  )
}

/** The words after a space's name in the switcher: its kind and the person's role. */
function spaceLabel(m: Schemas['TenantMembership']): string {
  if (m.tenant.kind === 'personal' && m.role === 'owner') return t('shell.space.personal', { name: m.tenant.name })
  return t('shell.space.role', { name: m.tenant.name, role: memberRoleLabels[m.role].toLowerCase() })
}

const NEW_TEAM = '__new_team'

/** The top bar, the navigation rail and the main region with the page (`children`). */
function Header({
  tenantId,
  isAdmin,
  unlisted,
  children,
}: {
  tenantId: string
  isAdmin: boolean
  unlisted: Schemas['TenantMembership'] | null
  children: ReactNode
}) {
  const { me, tenants } = useSession()
  const navigate = useNavigate()
  const location = useLocation()
  const [menuOpen, setMenuOpen] = useState(false)
  const [startTeam, setStartTeam] = useState(false)
  const [signOutError, setSignOutError] = useState<unknown>()
  // The search field shows the query only on the search page; it is empty everywhere else.
  const onSearch = location.pathname.endsWith('/search')
  const shownQuery = onSearch ? (new URLSearchParams(location.search).get('q') ?? '') : ''

  function switchTenant(id: string) {
    if (id === NEW_TEAM) {
      setStartTeam(true)
      return
    }
    // Keep the same section (files, agents, account) when it exists in the other space.
    const section = location.pathname.split('/')[3] ?? 'files'
    const keep = ['files', 'shared', 'trash', 'agents', 'account', 'search'].includes(section) ? section : 'files'
    navigate(tenantPath(id, keep))
  }

  function search(e: FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const query = new FormData(e.currentTarget).get('q')?.toString().trim()
    if (query) navigate(`${tenantPath(tenantId, 'search')}?q=${encodeURIComponent(query)}`)
  }

  async function signOut() {
    // Calls still under way answer 401 once the session ends: they must not race the browser's
    // way to the end-session page with a navigation to sign-in.
    setSigningOut(true)
    try {
      const res = await unwrap(client.POST('/v1/auth/logout'))
      browser.assign(res.logout_url)
    } catch (e) {
      setSigningOut(false)
      // The session had ended already: that is signed out too.
      if (e instanceof RuumApiError && e.status === 401) navigate('/signin', { replace: true })
      else setSignOutError(e)
    }
  }

  const link = (to: string, label: string) => (
    <NavItem to={tenantPath(tenantId, to)} onClick={() => setMenuOpen(false)}>
      {label}
    </NavItem>
  )

  const options = unlisted ? [...tenants, unlisted] : tenants

  return (
    <>
      <TopBar>
        <Brand to={tenantPath(tenantId, 'files')}>{t('app.name')}</Brand>
        <TopBarItem grow>
          <Field label={t('shell.space')} labelHidden>
            <Select value={tenantId} onChange={(e) => switchTenant(e.target.value)}>
              {options.map((m) => (
                <option key={m.tenant.id} value={m.tenant.id}>
                  {m === unlisted ? t('shell.space.shared', { name: m.tenant.name }) : spaceLabel(m)}
                </option>
              ))}
              <option value={NEW_TEAM}>{t('shell.space.startTeam')}</option>
            </Select>
          </Field>
        </TopBarItem>
        <SearchForm id="global-search" label={t('shell.search')} name="q" defaultValue={shownQuery} onSubmit={search} />
        <MenuToggle open={menuOpen} controls="main-nav" onToggle={() => setMenuOpen((o) => !o)} />
      </TopBar>
      <AppBody>
        <NavRail id="main-nav" label={t('shell.nav')} open={menuOpen}>
          <NavList>
            {link('files', t('shell.nav.files'))}
            {link('shared', t('shell.nav.shared'))}
            {link('trash', t('shell.nav.trash'))}
            {link('agents', t('shell.nav.agents'))}
            {isAdmin && link('admin', t('shell.nav.admin'))}
          </NavList>
          <NavList end>
            {link('account', me.user.display_name)}
            <NavButton onClick={signOut}>{t('shell.signOut')}</NavButton>
          </NavList>
          <ErrorNote error={signOutError} />
        </NavRail>
        <AppMain>{children}</AppMain>
      </AppBody>
      {startTeam && <StartTeamDialog onClose={() => setStartTeam(false)} />}
    </>
  )
}

/** "Start a team space" (copy.md 3.6): the person becomes the owner and invites people next. */
export function StartTeamDialog({ onClose }: { onClose: () => void }) {
  const { reloadTenants } = useSession()
  const { toast } = useToast()
  const navigate = useNavigate()
  const [name, setName] = useState('')
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  async function submit(e: FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(undefined)
    try {
      const created = await unwrap(client.POST('/v1/tenants', { body: { name: name.trim() } }))
      await reloadTenants()
      onClose()
      navigate(tenantPath(created.id, 'admin', 'invitations'))
      toast(t('shell.startTeam.done', { name: created.name }))
    } catch (err) {
      setError(err)
      setBusy(false)
    }
  }

  return (
    <Dialog title={t('shell.startTeam.title')} open onClose={onClose}>
      <Stack as="form" gap={4} onSubmit={submit}>
        <Field label={t('shell.startTeam.name')} hint={t('shell.startTeam.hint')}>
          <TextInput required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} placeholder={t('shell.startTeam.placeholder')} />
        </Field>
        <ErrorNote error={error} />
        <Actions>
          <Button onClick={onClose}>{t('common.cancel')}</Button>
          <Button type="submit" variant="primary" disabled={busy || !name.trim()} busy={busy}>
            {t('shell.startTeam.submit')}
          </Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}
