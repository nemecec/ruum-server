import { useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { client, RuumApiError, unwrap } from '../api/client'
import { tenantPath } from '../app/tenant'
import { useSession } from '../app/useSession'
import { useToast } from '../components/useToast'
import { ErrorNote } from '../components/ui'

/**
 * `/invitations/accept?token=...` (card UX-02): the link of an invitation email. The person signs
 * in first (RequireSession keeps the URL as `return_to`), sees what they join, and joins with one
 * click. Nothing happens without the click.
 */
export function AcceptInvitationPage() {
  const [params] = useSearchParams()
  const token = params.get('token') ?? ''
  const { me, reloadTenants } = useSession()
  const navigate = useNavigate()
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const [ended, setEnded] = useState(!token)
  const [busy, setBusy] = useState(false)

  async function join() {
    setBusy(true)
    setError(undefined)
    try {
      const member = await unwrap(client.POST('/v1/invitations/accept', { body: { token } }))
      const tenants = await reloadTenants()
      const space = tenants.find((t) => t.tenant.id === member.tenant_id)?.tenant.name ?? 'the space'
      navigate(tenantPath(member.tenant_id, 'files'), { replace: true })
      toast(`You've joined ${space}.`)
    } catch (e) {
      if (e instanceof RuumApiError && e.status === 404) setEnded(true)
      else setError(e)
      setBusy(false)
    }
  }

  if (ended) {
    return (
      <main className="page narrow" id="main">
        <h1>This invitation doesn't work any more</h1>
        <p>It was used already, or it ended. Ask the person who invited you for a new one.</p>
        <p>
          <Link to="/">Go to your files</Link>
        </p>
      </main>
    )
  }

  return (
    <main className="page narrow" id="main">
      <h1>You're invited to a space in Ruum</h1>
      <p>
        You join as <strong>{me.user.display_name}</strong> ({me.user.email}). The invitation email names the space, the person who
        invited you and your role; Ruum can't show them here yet. After you join, the space and what was shared with you show up in
        your space switcher.
      </p>
      <ErrorNote error={error} />
      <div className="actions left">
        <button type="button" className="primary" disabled={busy} onClick={join}>
          Join the space
        </button>
        <Link className="button" to="/">
          Not now
        </Link>
      </div>
    </main>
  )
}
