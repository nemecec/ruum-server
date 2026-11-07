import { useState } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { client, RuumApiError, unwrap } from '../api/client'
import { tenantPath } from '../app/tenant'
import { useSession } from '../app/useSession'
import { Actions, Button, ButtonLink, ErrorNote, Page, useToast } from '../ui'
import { t, tr } from '../i18n'

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
      const space = tenants.find((m) => m.tenant.id === member.tenant_id)?.tenant.name
      navigate(tenantPath(member.tenant_id, 'files'), { replace: true })
      toast(space ? t('invitation.joined', { space }) : t('invitation.joined.unnamed'))
    } catch (e) {
      if (e instanceof RuumApiError && e.status === 404) setEnded(true)
      else setError(e)
      setBusy(false)
    }
  }

  if (ended) {
    return (
      <Page narrow id="main">
        <h1>{t('invitation.ended.title')}</h1>
        <p>{t('invitation.ended.body')}</p>
        <p>
          <Link to="/">{t('common.goToFiles')}</Link>
        </p>
      </Page>
    )
  }

  return (
    <Page narrow id="main">
      <h1>{t('invitation.title')}</h1>
      <p>
        {tr('invitation.body', { name: me.user.display_name, email: me.user.email, b: (c) => <strong>{c}</strong> })}
      </p>
      <ErrorNote error={error} />
      <Actions align="start">
        <Button variant="primary" disabled={busy} busy={busy} onClick={join}>
          {t('invitation.join')}
        </Button>
        <ButtonLink to="/">{t('common.notNow')}</ButtonLink>
      </Actions>
    </Page>
  )
}
