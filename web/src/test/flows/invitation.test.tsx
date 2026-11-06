import { screen, waitFor } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { renderApp } from '../render'
import { recordRequests } from '../requests'

describe('accept an invitation (/invitations/accept)', () => {
  it('joins the space with one click and lands in its files', async () => {
    const seen = recordRequests()
    const { user, router } = renderApp('/invitations/accept?token=demo-invite')
    expect(await screen.findByRole('heading', { name: "You're invited to a space in Ruum" })).toBeInTheDocument()
    // Nothing happens without the click.
    expect(seen.some((r) => r.path === '/v1/invitations/accept')).toBe(false)
    await user.click(screen.getByRole('button', { name: 'Join the space' }))
    expect(await screen.findByText("You've joined Lovelace Lab.")).toBeInTheDocument()
    await waitFor(() => expect(router.state.location.pathname).toMatch(/^\/t\/[0-9a-f-]{36}\/files$/))
    // React Router commits the new location in a transition: router.state changes first.
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Space' })).toHaveDisplayValue('Lovelace Lab (member)'))
  })

  it('says so when the invitation was used or ended', async () => {
    const { user } = renderApp('/invitations/accept?token=used-already')
    await user.click(await screen.findByRole('button', { name: 'Join the space' }))
    expect(await screen.findByRole('heading', { name: "This invitation doesn't work any more" })).toBeInTheDocument()
  })

  it('sends a person without a session through sign-in and back to the invitation', async () => {
    db.signedIn = false
    renderApp('/invitations/accept?token=demo-invite')
    const link = await screen.findByRole('link', { name: 'Continue with Google' })
    const returnTo = new URL(link.getAttribute('href')!, 'http://x').searchParams.get('return_to')
    expect(returnTo).toBe('/invitations/accept?token=demo-invite')
  })
})
