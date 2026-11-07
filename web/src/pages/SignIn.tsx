import { useSearchParams } from 'react-router'
import { loginUrl, safeReturnTo, type Idp } from '../app/auth'
import { ButtonAnchor, Note, Page, Stack, Text } from '../ui'
import { t, type MessageKey } from '../i18n'

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

/** The sign-in page. Each button starts `loginStart` with a full page load. */
export function SignInPage() {
  const [params] = useSearchParams()
  const returnTo = safeReturnTo(params.get('return_to'))
  const error = params.get('error')
  const signedOut = params.has('signed_out')
  const invited = returnTo.startsWith('/invitations/accept')

  return (
    <Page narrow id="main">
      <h1>{t('signIn.title')}</h1>
      {invited ? (
        <p>{t('signIn.invited')}</p>
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
