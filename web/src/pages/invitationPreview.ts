import { useEffect, useState } from 'react'
import { client, RuumApiError, unwrap, type Schemas } from '../api/client'
import { fragmentToken, INVITATION_TOKEN_KEY } from '../app/auth'

export type InvitationPreview = Schemas['InvitationPreview']

/** What the page knows of an invitation token (`previewInvitation`, C-9, decision 0003-6). */
export type PreviewState =
  | { kind: 'loading' }
  | { kind: 'found'; preview: InvitationPreview }
  /** The server does not know the token (`404`): it never existed, or it ended long ago. */
  | { kind: 'unknown' }
  /** The preview did not answer (network, `429`): the page works without it. */
  | { kind: 'failed' }

/**
 * The token of an invitation link: the fragment (`#token=`) first, then the one kept over sign-in,
 * then `?token=` of `search` (old mails).
 */
export function invitationToken(search: URLSearchParams, hash = window.location.hash): string {
  const fromHash = fragmentToken(hash)
  if (fromHash) return fromHash
  try {
    const kept = sessionStorage.getItem(INVITATION_TOKEN_KEY)
    if (kept) return kept
  } catch {
    // Storage can be off.
  }
  return search.get('token') ?? ''
}

/** Forgets the token kept over sign-in. */
export function forgetInvitationToken(): void {
  try {
    sessionStorage.removeItem(INVITATION_TOKEN_KEY)
  } catch {
    // Storage can be off.
  }
}

/** Asks the server what the invitation of `token` is for. It needs no sign-in and changes nothing. */
export function useInvitationPreview(token: string): PreviewState {
  const [state, setState] = useState<{ token: string; value: PreviewState }>()
  useEffect(() => {
    if (!token) return
    let active = true
    unwrap(client.POST('/v1/invitations/preview', { body: { token } }))
      .then((preview) => active && setState({ token, value: { kind: 'found', preview } }))
      .catch((e: unknown) => {
        if (!active) return
        const unknown = e instanceof RuumApiError && e.status === 404
        setState({ token, value: { kind: unknown ? 'unknown' : 'failed' } })
      })
    return () => {
      active = false
    }
  }, [token])
  if (!token) return { kind: 'unknown' }
  return state?.token === token ? state.value : { kind: 'loading' }
}

/**
 * The masked form of an email, as the server masks it (`k***@e***.ee`), to tell whether the
 * invitation went to the signed-in address.
 */
export function maskEmail(email: string): string {
  const first = (s: string) => {
    const c = [...s][0]
    return c ? `${c}***` : '***'
  }
  const at = email.trim().lastIndexOf('@')
  if (at < 0) return '***'
  const name = email.trim().slice(0, at)
  const domain = email.trim().slice(at + 1)
  const dot = domain.lastIndexOf('.')
  const host = dot >= 0 && dot < domain.length - 1 ? `${first(domain.slice(0, dot))}.${domain.slice(dot + 1)}` : first(domain)
  return `${first(name)}@${host}`
}
