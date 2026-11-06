import { useEffect, type ReactNode } from 'react'
import { Navigate, useLocation, useNavigate } from 'react-router'
import { client, onUnauthenticated, RuumApiError, unwrap } from '../api/client'
import { useApi } from '../api/useApi'
import { rememberName } from '../components/format'
import { ErrorNote, Loading } from '../components/ui'
import { signInPath } from './auth'
import { SessionContext } from './useSession'


async function loadSession() {
  const [me, tenants] = await Promise.all([unwrap(client.GET('/v1/me')), unwrap(client.GET('/v1/tenants'))])
  rememberName(me.user.id, me.user.display_name)
  return { me, tenants: tenants.items }
}

/** Loads the signed-in user and the tenants; sends the browser to sign-in without a session. */
export function RequireSession({ children }: { children: ReactNode }) {
  const location = useLocation()
  const navigate = useNavigate()
  const state = useApi(loadSession, [])

  // A 401 on any later call (an expired session) also goes to sign-in.
  useEffect(
    () => onUnauthenticated(() => navigate(signInPath(window.location), { replace: true })),
    [navigate],
  )

  if (state.error instanceof RuumApiError && state.error.status === 401) {
    return <Navigate to={signInPath(location)} replace />
  }
  if (state.error) return <main className="page"><ErrorNote error={state.error} /></main>
  if (!state.data) return <main className="page"><Loading label="Signing in" /></main>
  return (
    <SessionContext.Provider value={{ ...state.data, reloadTenants: state.reload }}>{children}</SessionContext.Provider>
  )
}
