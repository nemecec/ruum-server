import { useState, type FormEvent } from 'react'
import { Navigate, NavLink, Outlet, useLocation, useNavigate, useParams } from 'react-router'
import { client, errorText, setTenant, unwrap } from '../api/client'
import { memberRoleLabels } from '../components/format'
import { browser } from './browser'
import { useSession } from './useSession'
import { lastTenant, rememberTenant, TenantContext, tenantPath } from './tenant'

/** `/`: goes to the files of the last tenant, or of the home tenant. */
export function HomeRedirect() {
  const { me, tenants } = useSession()
  const last = lastTenant()
  const id = tenants.some((t) => t.tenant.id === last) ? last! : me.user.home_tenant_id
  return <Navigate to={tenantPath(id, 'files')} replace />
}

/** `/account` without a tenant goes to the account page of the last or home tenant. */
export function AccountRedirect() {
  const { me, tenants } = useSession()
  const last = lastTenant()
  const id = tenants.some((t) => t.tenant.id === last) ? last! : me.user.home_tenant_id
  return <Navigate to={tenantPath(id, 'account')} replace />
}

/** The layout of every page inside a tenant: the header, the navigation and the page. */
export function TenantLayout() {
  const { tenantId = '' } = useParams()
  const { tenants } = useSession()
  const membership = tenants.find((t) => t.tenant.id === tenantId)

  if (!membership) {
    return (
      <main className="page">
        <h1>Tenant not found</h1>
        <p>You are not a member of this tenant, or it does not exist.</p>
        <p>
          <NavLink to="/">Go to your files</NavLink>
        </p>
      </main>
    )
  }

  // Set the tenant header before the pages below load their data (their effects run first).
  setTenant(tenantId)
  rememberTenant(tenantId)
  const isAdmin = membership.role === 'admin' || membership.role === 'owner'

  return (
    <TenantContext.Provider value={{ tenant: membership.tenant, role: membership.role, isAdmin }}>
      <a className="skip" href="#main">
        Skip to content
      </a>
      <Header tenantId={tenantId} isAdmin={isAdmin} />
      <main id="main" className="main" tabIndex={-1}>
        <Outlet key={tenantId} />
      </main>
    </TenantContext.Provider>
  )
}

function Header({ tenantId, isAdmin }: { tenantId: string; isAdmin: boolean }) {
  const { me, tenants } = useSession()
  const navigate = useNavigate()
  const location = useLocation()
  const [menuOpen, setMenuOpen] = useState(false)
  const [signOutError, setSignOutError] = useState<string | null>(null)

  function switchTenant(id: string) {
    // Keep the same section (files, agents, admin) when it exists in the other tenant.
    const section = location.pathname.split('/')[3] ?? 'files'
    const keep = ['files', 'shared', 'trash', 'agents', 'account', 'search'].includes(section) ? section : 'files'
    navigate(tenantPath(id, keep))
  }

  function search(e: FormEvent<HTMLFormElement>) {
    e.preventDefault()
    const q = new FormData(e.currentTarget).get('q')?.toString().trim()
    if (q) navigate(`${tenantPath(tenantId, 'search')}?q=${encodeURIComponent(q)}`)
  }

  async function signOut() {
    try {
      const res = await unwrap(client.POST('/v1/auth/logout'))
      browser.assign(res.logout_url)
    } catch (e) {
      setSignOutError(errorText(e))
    }
  }

  const link = (to: string, label: string) => (
    <li>
      <NavLink to={tenantPath(tenantId, to)} onClick={() => setMenuOpen(false)}>
        {label}
      </NavLink>
    </li>
  )

  return (
    <header className="topbar">
      <div className="topbar-row">
        <NavLink to={tenantPath(tenantId, 'files')} className="brand">
          Ruum
        </NavLink>
        <label className="tenant-switcher">
          <span className="sr-only">Tenant</span>
          <select value={tenantId} onChange={(e) => switchTenant(e.target.value)}>
            {tenants.map((t) => (
              <option key={t.tenant.id} value={t.tenant.id}>
                {t.tenant.name} ({t.tenant.kind === 'personal' ? 'personal' : memberRoleLabels[t.role].toLowerCase()})
              </option>
            ))}
          </select>
        </label>
        <form role="search" className="search" onSubmit={search}>
          <label className="sr-only" htmlFor="global-search">
            Search by name
          </label>
          <input id="global-search" name="q" type="search" placeholder="Search by name" />
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
          {isAdmin && link('admin', 'Admin')}
        </ul>
        <ul className="nav-end">
          {link('account', me.user.display_name)}
          <li>
            <button type="button" className="link" onClick={signOut}>
              Sign out
            </button>
          </li>
        </ul>
        {signOutError && (
          <p className="error" role="alert">
            {signOutError}
          </p>
        )}
      </nav>
    </header>
  )
}
