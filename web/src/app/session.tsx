import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { Navigate, useLocation, useNavigate } from 'react-router'
import { client, onUnauthenticated, RuumApiError, unwrap, type Schemas } from '../api/client'
import { useApi } from '../api/useApi'
import { ErrorNote, Loading, Page } from '../ui'
import { UploadsProvider } from '../pages/files/uploads'
import { signInPath } from './auth'
import { SessionContext } from './useSession'
import { t, useLocale } from '../i18n'

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

  // The language stored with the account wins over the one of this browser (design 12.4).
  // Only a new answer of getMe decides; a change on the Account page sets both.
  const { preference, setPreference } = useLocale()
  const stored = state.data ? (state.data.me.preferences?.locale ?? null) : undefined
  const applied = useRef<string | null | undefined>(undefined)
  useEffect(() => {
    if (stored === undefined || applied.current === stored) return
    applied.current = stored
    if (stored !== preference) setPreference(stored)
  }, [stored, preference, setPreference])

  const reloadTenants = useCallback(async () => {
    const res = await unwrap(client.GET('/v1/tenants'))
    setTenants(res.items)
    return res.items
  }, [])

  if (state.error instanceof RuumApiError && state.error.status === 401) {
    return <Navigate to={signInPath(location)} replace />
  }
  if (state.error)
    return (
      <Page>
        <ErrorNote error={state.error} />
      </Page>
    )
  if (!state.data)
    return (
      <Page>
        <Loading label={t('session.signingIn')} />
      </Page>
    )
  return (
    <SessionContext.Provider value={{ me: state.data.me, tenants: tenants ?? state.data.tenants, reloadTenants }}>
      <UploadsProvider>{children}</UploadsProvider>
    </SessionContext.Provider>
  )
}
