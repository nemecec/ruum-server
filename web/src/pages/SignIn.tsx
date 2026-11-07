import { useSearchParams } from 'react-router'
import { loginUrl, safeReturnTo, type Idp } from '../app/auth'
import { ButtonAnchor, Note, Page, Stack, Text } from '../ui'
import { t, type MessageKey } from '../i18n'
import { invitationToken, useInvitationPreview, type PreviewState } from './invitationPreview'

const providers: { idp: Idp; label: MessageKey }[] = [
  { idp: 'passkey', label: 'signIn.email' },
  { idp: 'google', label: 'signIn.google' },
  { idp: 'microsoft', label: 'signIn.microsoft' },
]

/** Error codes of the identity provider (OIDC) that the callback can pass back. */
const errorTexts: Record<string, MessageKey> = {
  access_denied: 'signIn.error.cancelled',
  login_required: 'signIn.error.again',
}

/** The invitation line: names the space and the address while the invitation is open. */
function invitedText(preview: PreviewState): string {
  const p = preview.kind === 'found' ? preview.preview : null
  if (p?.state === 'pending' && p.tenant_name && p.email_masked) return t('signIn.invitedTo', { space: p.tenant_name, email: p.email_masked })
  return t('signIn.invited')
}

/** The sign-in page. Each button starts `loginStart` with a full page load. */
export function SignInPage() {
  const [params] = useSearchParams()
  const returnTo = safeReturnTo(params.get('return_to'))
  const error = params.get('error')
  const signedOut = params.has('signed_out')
  const invited = returnTo.startsWith('/invitations/accept')
  // The space of the invitation, before sign-in (`previewInvitation`, C-9): the token waits in
  // session storage (the link's fragment), or is in `return_to` (`?token=` of old mails).
  const preview = useInvitationPreview(invited ? invitationToken(new URLSearchParams(returnTo.split('?')[1] ?? ''), '') : '')

  return (
    <Page narrow id="main">
      <h1>{t('signIn.title')}</h1>
      {invited ? (
        <p>{invitedText(preview)}</p>
      ) : (
        <Text tone="muted">{t('signIn.tagline')}</Text>
      )}
      {signedOut && (
        <Note tone="success" role="status">
          {t('signIn.signedOut')}
        </Note>
      )}
      {error && (
        <Note tone="danger" role="alert">
          {errorTexts[error] ? t(errorTexts[error]) : t('signIn.error.failed', { error })}
        </Note>
      )}
      <Stack as="ul" gap={2}>
        {providers.map((p, i) => (
          <li key={p.idp}>
            <ButtonAnchor wide variant={i === 0 ? 'primary' : 'secondary'} href={loginUrl(returnTo, p.idp)}>
              {t(p.label)}
            </ButtonAnchor>
          </li>
        ))}
      </Stack>
      <Text tone="muted" size="sm">
        {t('signIn.newHere')}
      </Text>
    </Page>
  )
}
