import { useState, type ReactNode } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'
import { client, RuumApiError, unwrap } from '../api/client'
import { tenantPath } from '../app/tenant'
import { useSession } from '../app/useSession'
import { formatDay } from '../components/format'
import { Actions, Button, ButtonLink, ErrorNote, Loading, Note, Page, Stack, Text, useToast } from '../ui'
import { t, tr } from '../i18n'
import { forgetInvitationToken, invitationToken, maskEmail, useInvitationPreview, type InvitationPreview } from './invitationPreview'

/**
 * `/invitations/accept#token=...` (card UX-02, decisions 0003-3 and 0003-6): the link of an
 * invitation email. The person signs in first (RequireSession keeps the page as `return_to`, the
 * token waits in session storage), sees what they join (`previewInvitation`: the space, who
 * invited them, the role and the end date), and joins with one click. Nothing happens without the
 * click. Old mails carry `?token=`.
 */
export function AcceptInvitationPage() {
  const [params] = useSearchParams()
  const [token] = useState(() => invitationToken(params))
  const preview = useInvitationPreview(token)
  const { me, reloadTenants } = useSession()
  const navigate = useNavigate()
  const { toast } = useToast()
  const [error, setError] = useState<unknown>()
  const [gone, setGone] = useState(false)
  const [busy, setBusy] = useState(false)

  async function join() {
    setBusy(true)
    setError(undefined)
    try {
      const member = await unwrap(client.POST('/v1/invitations/accept', { body: { token } }))
      forgetInvitationToken()
      const tenants = await reloadTenants()
      const space = tenants.find((m) => m.tenant.id === member.tenant_id)?.tenant.name
      navigate(tenantPath(member.tenant_id, 'files'), { replace: true })
      toast(space ? t('invitation.joined', { space }) : t('invitation.joined.unnamed'))
    } catch (e) {
      if (e instanceof RuumApiError && e.status === 404) {
        forgetInvitationToken()
        setGone(true)
      } else setError(e)
      setBusy(false)
    }
  }

  if (preview.kind === 'loading') {
    return (
      <Page narrow id="main">
        <Loading />
      </Page>
    )
  }
  if (gone || preview.kind === 'unknown') return <Ended title={t('invitation.ended.title')} body={t('invitation.ended.body')} />
  const p = preview.kind === 'found' ? preview.preview : null
  if (p?.state === 'expired') {
    return (
      <Ended
        title={p.tenant_name ? t('invitation.expired.title', { space: p.tenant_name }) : t('invitation.ended.title')}
        body={
          p.inviter_display_name
            ? t('invitation.expired.body', { date: p.expires_at ? formatDay(p.expires_at) : '-', inviter: p.inviter_display_name })
            : t('invitation.ended.body')
        }
      />
    )
  }
  if (p?.state === 'revoked') return <Ended title={t('invitation.ended.title')} body={t('invitation.revoked.body')} />
  if (p?.state === 'accepted') return <Ended title={t('invitation.ended.title')} body={t('invitation.accepted.body')} />

  // Pending, or the preview did not answer: the page still lets the person join.
  // Data, not text: emails compare without case.
  const otherAddress = p?.email_masked && p.email_masked.toLowerCase() !== maskEmail(me.user.email).toLowerCase() // i18n-ignore
  return (
    <Page narrow id="main">
      <h1>{p ? title(p) : t('invitation.title')}</h1>
      {p ? (
        <Stack gap={2}>
          <p>{roleText(p)}</p>
          {p.expires_at && <Text tone="muted">{t('invitation.preview.ends', { date: formatDay(p.expires_at) })}</Text>}
        </Stack>
      ) : null}
      <p>
        {p
          ? tr('invitation.preview.as', { name: me.user.display_name, email: me.user.email, b: (c) => <strong>{c}</strong> })
          : tr('invitation.body', { name: me.user.display_name, email: me.user.email, b: (c) => <strong>{c}</strong> })}
      </p>
      {otherAddress && (
        <Note tone="warning" role="note">
          {t('invitation.preview.otherAddress', { email: p.email_masked ?? '' })}
        </Note>
      )}
      <ErrorNote error={error} />
      <Actions align="start">
        <Button variant="primary" disabled={busy} busy={busy} onClick={join}>
          {p?.tenant_name ? t('invitation.join.named', { space: p.tenant_name }) : t('invitation.join')}
        </Button>
        <ButtonLink to="/">{t('common.notNow')}</ButtonLink>
      </Actions>
    </Page>
  )
}

/** "Charles Babbage invited you to Lovelace Lab", or without the inviter's name. */
function title(p: InvitationPreview): string {
  const space = p.tenant_name ?? ''
  return p.inviter_display_name ? t('invitation.preview.title', { inviter: p.inviter_display_name, space }) : t('invitation.preview.title.noInviter', { space })
}

/** What the role of the invitation lets the person do in the space. */
function roleText(p: InvitationPreview): string {
  const space = p.tenant_name ?? ''
  switch (p.role) {
    case 'guest':
      return t('invitation.preview.guest', { space })
    case 'admin':
    case 'owner':
      return t('invitation.preview.admin', { space })
    default:
      return t('invitation.preview.member', { role: p.content_role ?? 'manage', space })
  }
}

function Ended({ title, body }: { title: string; body: ReactNode }) {
  return (
    <Page narrow id="main">
      <h1>{title}</h1>
      <p>{body}</p>
      <p>
        <Link to="/">{t('common.goToFiles')}</Link>
      </p>
    </Page>
  )
}
