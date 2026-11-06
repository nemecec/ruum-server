import { useCallback, useEffect, useState, type ReactNode } from 'react'
import { Navigate, useLocation, useNavigate } from 'react-router'
import { client, onUnauthenticated, RuumApiError, unwrap, type Schemas } from '../api/client'
import { useApi } from '../api/useApi'
import { ErrorNote, Loading } from '../components/ui'
import { UploadsProvider } from '../pages/files/uploads'
import { signInPath } from './auth'
import { SessionContext } from './useSession'

async function loadSession() {
  const [me, tenants] = await Promise.all([unwrap(client.GET('/v1/me')), unwrap(client.GET('/v1/tenants'))])
  return { me, tenants: tenants.items }
}

/** Loads the signed-in user and the spaces; sends the browser to sign-in without a session. */
export function RequireSession({ children }: { children: ReactNode }) {
  const location = useLocation()
  const navigate = useNavigate()
  const state = useApi(loadSession, [])
  const [tenants, setTenants] = useState<Schemas['TenantMembership'][] | null>(null)

  // A 401 on a later call (an expired session) also goes to sign-in, with the page to come back
  // to. The first load answers 401 for /v1/me and /v1/tenants at once: the second answer must not
  // send the browser on from /signin, which would drop `return_to`.
  useEffect(
    () =>
      onUnauthenticated(() => {
        if (window.location.pathname.startsWith('/signin')) return
        navigate(signInPath(window.location), { replace: true })
      }),
    [navigate],
  )

  const reloadTenants = useCallback(async () => {
    const res = await unwrap(client.GET('/v1/tenants'))
    setTenants(res.items)
    return res.items
  }, [])

  if (state.error instanceof RuumApiError && state.error.status === 401) {
    return <Navigate to={signInPath(location)} replace />
  }
  if (state.error) return <main className="page"><ErrorNote error={state.error} /></main>
  if (!state.data) return <main className="page"><Loading label="Signing in" /></main>
  return (
    <SessionContext.Provider value={{ me: state.data.me, tenants: tenants ?? state.data.tenants, reloadTenants }}>
      <UploadsProvider>{children}</UploadsProvider>
    </SessionContext.Provider>
  )
}
