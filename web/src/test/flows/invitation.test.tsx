import { screen, waitFor } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { renderApp } from '../render'
import { recordRequests } from '../requests'

describe('accept an invitation (/invitations/accept)', () => {
  it('names the space, the inviter and the role, and joins with one click (C-9)', async () => {
    const seen = recordRequests()
    const { user, router } = renderApp('/invitations/accept?token=demo-invite')
    expect(await screen.findByRole('heading', { name: 'Charles Babbage invited you to Lovelace Lab', level: 1 })).toBeInTheDocument()
    expect(
      screen.getByText('As a member who can edit, you change files and folders in Lovelace Lab. You can\'t move, delete or restore them.'),
    ).toBeInTheDocument()
    expect(screen.getByText(/^The invitation is open until /)).toBeInTheDocument()
    // The invitation went to Ada's address (masked alike): no warning.
    expect(screen.queryByRole('note')).not.toBeInTheDocument()
    // The preview changes nothing, and nothing happens without the click.
    expect(seen.find((r) => r.path === '/v1/invitations/preview')?.body).toEqual({ token: 'demo-invite' })
    expect(seen.some((r) => r.path === '/v1/invitations/accept')).toBe(false)
    await user.click(screen.getByRole('button', { name: 'Join Lovelace Lab' }))
    expect(await screen.findByText("You're in! Welcome to Lovelace Lab.")).toBeInTheDocument()
    await waitFor(() => expect(router.state.location.pathname).toMatch(/^\/t\/[0-9a-f-]{36}\/files$/))
    // React Router commits the new location in a transition: router.state changes first.
    await waitFor(() => expect(screen.getByRole('button', { name: 'Lovelace Lab (change space)' })).toBeInTheDocument())
  })

  it('says when the invitation went to another address', async () => {
    renderApp('/invitations/accept?token=demo-other-address')
    expect(await screen.findByRole('heading', { name: 'Charles Babbage invited you to Lovelace Lab' })).toBeInTheDocument()
    expect(screen.getByText('As a guest, you see only what people in Lovelace Lab share with you.')).toBeInTheDocument()
    expect(screen.getByRole('note')).toHaveTextContent('The invitation was sent to l***@m***.org, not to the address you signed in with.')
    expect(screen.getByRole('button', { name: 'Join Lovelace Lab' })).toBeInTheDocument()
  })

  it('says so at once when the token is unknown, without a Join button', async () => {
    renderApp('/invitations/accept?token=used-already')
    expect(await screen.findByRole('heading', { name: "This invitation doesn't work any more" })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /^Join/ })).not.toBeInTheDocument()
  })

  it('names the space and the inviter of an expired invitation', async () => {
    renderApp('/invitations/accept?token=demo-expired')
    expect(await screen.findByRole('heading', { name: 'The invitation to Lovelace Lab has ended' })).toBeInTheDocument()
    expect(screen.getByText(/^It was open until .+\. Ask Charles Babbage for a new one\.$/)).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /^Join/ })).not.toBeInTheDocument()
  })

  it('names nothing of a revoked or used invitation', async () => {
    renderApp('/invitations/accept?token=demo-revoked')
    expect(await screen.findByRole('heading', { name: "This invitation doesn't work any more" })).toBeInTheDocument()
    expect(screen.getByText('The person who invited you took it back. Ask them if you think this is a mistake.')).toBeInTheDocument()
    expect(screen.queryByText(/Lovelace Lab/)).not.toBeInTheDocument()
  })

  it('says a used invitation was used', async () => {
    renderApp('/invitations/accept?token=demo-used')
    expect(await screen.findByText('It was used already. If you accepted it, the space is in your space switcher.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /^Join/ })).not.toBeInTheDocument()
  })

  it('names the space on the sign-in page, before sign-in', async () => {
    db.signedIn = false
    renderApp('/invitations/accept?token=demo-invite')
    expect(
      await screen.findByText('Sign in, or make a free account, to join Lovelace Lab. The invitation was sent to a***@e***.com. Ruum brings you back to it.'),
    ).toBeInTheDocument()
  })

  it('sends a person without a session through sign-in and back to the invitation', async () => {
    db.signedIn = false
    renderApp('/invitations/accept?token=demo-invite')
    const link = await screen.findByRole('link', { name: 'Continue with Google' })
    const returnTo = new URL(link.getAttribute('href')!, 'http://x').searchParams.get('return_to')
    expect(returnTo).toBe('/invitations/accept?token=demo-invite')
  })
})
