import { useSearchParams } from 'react-router'
import { loginUrl, safeReturnTo, type Idp } from '../app/auth'
import { ButtonAnchor, Note, Page, Stack, Text } from '../ui'

const providers: { idp: Idp; label: string }[] = [
  { idp: 'passkey', label: 'Sign in with email' },
  { idp: 'google', label: 'Continue with Google' },
  { idp: 'microsoft', label: 'Continue with Microsoft' },
]

/** Error codes of the identity provider (OIDC) that the callback can pass back. */
const errorTexts: Record<string, string> = {
  access_denied: 'Sign-in was cancelled.',
  login_required: 'Sign in again.',
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
      <h1>Sign in to Ruum</h1>
      {invited ? (
        <p>Sign in, or make a free account, to join the space you were invited to. Ruum brings you back to the invitation.</p>
      ) : (
        <Text tone="muted">Files, databases and apps in one tree, for you and your agents.</Text>
      )}
      {signedOut && (
        <Note tone="success" role="status">
          You are signed out.
        </Note>
      )}
      {error && (
        <Note tone="danger" role="alert">
          {errorTexts[error] ?? `Sign-in failed (${error}). Try again.`}
        </Note>
      )}
      <Stack as="ul" gap={2}>
        {providers.map((p, i) => (
          <li key={p.idp}>
            <ButtonAnchor wide variant={i === 0 ? 'primary' : 'secondary'} href={loginUrl(returnTo, p.idp)}>
              {p.label}
            </ButtonAnchor>
          </li>
        ))}
      </Stack>
      <Text tone="muted" size="sm">
        New here? Signing in makes your account and a personal space.
      </Text>
    </Page>
  )
}
