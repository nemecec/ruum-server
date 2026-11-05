import { useEffect, useState } from 'react'
import { api, RuumApiError, type Schemas } from './api/client'

type Status =
  | { state: 'loading' }
  | { state: 'ok'; health: Schemas['Health'] }
  | { state: 'error'; message: string }

// Placeholder page. The real UI (design section 12.3) is built by the web UI work area.
export default function App() {
  const [status, setStatus] = useState<Status>({ state: 'loading' })

  useEffect(() => {
    api<Schemas['Health']>('/healthz')
      .then((health) => setStatus({ state: 'ok', health }))
      .catch((e: unknown) =>
        setStatus({
          state: 'error',
          message: e instanceof RuumApiError ? `${e.code}: ${e.message}` : String(e),
        }),
      )
  }, [])

  return (
    <main className="page">
      <h1>Ruum</h1>
      <p>Files, photos, databases and apps in one tree.</p>
      <p className="status">
        Core service:{' '}
        {status.state === 'loading' && 'checking...'}
        {status.state === 'ok' && `${status.health.status} (version ${status.health.version})`}
        {status.state === 'error' && `not reachable (${status.message})`}
      </p>
      <p>
        <a href="/v1/auth/login">Sign in</a>
      </p>
    </main>
  )
}
