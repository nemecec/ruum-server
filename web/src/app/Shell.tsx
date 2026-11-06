import { useState, type FormEvent } from 'react'
import { Navigate, NavLink, Outlet, useLocation, useNavigate, useParams } from 'react-router'
import { client, setTenant, unwrap, type Schemas } from '../api/client'
import { memberRoleLabels } from '../components/format'
import { useToast } from '../components/useToast'
import { Dialog, ErrorNote } from '../components/ui'
import { browser } from './browser'
import { NamesProvider } from './names'
import { useSession } from './useSession'
import { lastTenant, rememberTenant, TenantContext, tenantPath } from './tenant'

/** `/`: goes to the files of the last space, or of the personal space. */
export function HomeRedirect() {
  const { me, tenants } = useSession()
  const last = lastTenant()
  const id = tenants.some((t) => t.tenant.id === last) ? last! : me.user.home_tenant_id
  return <Navigate to={tenantPath(id, 'files')} replace />
}

/** `/account` without a space goes to the account page of the last or personal space. */
export function AccountRedirect() {
  const { me, tenants } = useSession()
  const last = lastTenant()
  const id = tenants.some((t) => t.tenant.id === last) ? last! : me.user.home_tenant_id
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
      name: 'A shared space',
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
  const listed = tenants.find((t) => t.tenant.id === tenantId)
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
        <a className="skip" href="#main">
          Skip to content
        </a>
        <Header tenantId={tenantId} isAdmin={isAdmin} unlisted={listed ? null : membership} />
        <main id="main" className="main" tabIndex={-1}>
          {/* Only for a guest of the space. A space that is not in the person's list is not theirs at
              all yet: the pages say so (NotInSpaceYet), and a banner that says "guest" would contradict them. */}
          {listed?.role === 'guest' && (
            <p className="note" role="note">
              You're a guest here. You see only what people shared with you.
            </p>
          )}
          <Outlet key={tenantId} />
        </main>
      </NamesProvider>
    </TenantContext.Provider>
  )
}

/** The words after a space's name in the switcher: its kind and the person's role. */
function spaceLabel(t: Schemas['TenantMembership']): string {
  if (t.tenant.kind === 'personal' && t.role === 'owner') return `${t.tenant.name} (personal)`
  return `${t.tenant.name} (${memberRoleLabels[t.role].toLowerCase()})`
}

const NEW_TEAM = '__new_team'

function Header({ tenantId, isAdmin, unlisted }: { tenantId: string; isAdmin: boolean; unlisted: Schemas['TenantMembership'] | null }) {
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
    try {
      const res = await unwrap(client.POST('/v1/auth/logout'))
      browser.assign(res.logout_url)
    } catch (e) {
      setSignOutError(e)
    }
  }

  const link = (to: string, label: string) => (
    <li>
      <NavLink to={tenantPath(tenantId, to)} onClick={() => setMenuOpen(false)}>
        {label}
      </NavLink>
    </li>
  )

  const options = unlisted ? [...tenants, unlisted] : tenants

  return (
    <header className="topbar">
      <div className="topbar-row">
        <NavLink to={tenantPath(tenantId, 'files')} className="brand">
          Ruum
        </NavLink>
        <label className="tenant-switcher">
          <span className="sr-only">Space</span>
          <select value={tenantId} onChange={(e) => switchTenant(e.target.value)}>
            {options.map((t) => (
              <option key={t.tenant.id} value={t.tenant.id}>
                {t === unlisted ? `${t.tenant.name} (shared with you)` : spaceLabel(t)}
              </option>
            ))}
            <option value={NEW_TEAM}>Start a team space...</option>
          </select>
        </label>
        <form role="search" className="search" onSubmit={search}>
          <label className="sr-only" htmlFor="global-search">
            Search by name
          </label>
          <input key={shownQuery} id="global-search" name="q" type="search" placeholder="Search by name" defaultValue={shownQuery} />
        </form>
        <button
          type="button"
          className="menu-toggle"
          aria-expanded={menuOpen}
          aria-controls="main-nav"
          onClick={() => setMenuOpen((o) => !o)}
        >
          Menu
        </button>
      </div>
      <nav id="main-nav" aria-label="Main" className={menuOpen ? 'nav open' : 'nav'}>
        <ul>
          {link('files', 'Files')}
          {link('shared', 'Shared with me')}
          {link('trash', 'Trash')}
          {link('agents', 'Agents')}
          {isAdmin && link('admin', 'Space settings')}
        </ul>
        <ul className="nav-end">
          {link('account', me.user.display_name)}
          <li>
            <button type="button" className="link" onClick={signOut}>
              Sign out
            </button>
          </li>
        </ul>
        <ErrorNote error={signOutError} />
      </nav>
      {startTeam && <StartTeamDialog onClose={() => setStartTeam(false)} />}
    </header>
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
      const t = await unwrap(client.POST('/v1/tenants', { body: { name: name.trim() } }))
      await reloadTenants()
      onClose()
      navigate(tenantPath(t.id, 'admin', 'invitations'))
      toast(`${t.name} is ready. Invite the people you work with.`)
    } catch (err) {
      setError(err)
      setBusy(false)
    }
  }

  return (
    <Dialog title="Start a team space" open onClose={onClose}>
      <form className="stack" onSubmit={submit}>
        <label>
          Team name
          <input required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} placeholder="Pärn Studio" />
        </label>
        <p className="muted small">You'll be the owner. You can invite people next.</p>
        <ErrorNote error={error} />
        <div className="actions">
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy || !name.trim()}>
            Create team space
          </button>
        </div>
      </form>
    </Dialog>
  )
}
