import { screen, within } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { loginUrl, safeReturnTo } from '../../app/auth'
import { db } from '../../mocks/db'
import { personal, renderApp } from '../render'
import { recordRequests } from '../requests'

describe('sign-in', () => {
  it('sends a visitor without a session to sign-in and keeps the page as return_to', async () => {
    db.signedIn = false
    const { router } = renderApp(`${personal}/agents`)
    expect(await screen.findByRole('heading', { name: 'Sign in to Ruum' })).toBeInTheDocument()
    expect(router.state.location.pathname).toBe('/signin')
    expect(router.state.location.search).toBe(`?return_to=${encodeURIComponent(`${personal}/agents`)}`)

    // Each provider starts loginStart with the same return_to and its idp hint.
    const google = screen.getByRole('link', { name: 'Continue with Google' })
    expect(google).toHaveAttribute('href', `/v1/auth/login?return_to=${encodeURIComponent(`${personal}/agents`)}&idp=google`)
    expect(screen.getByRole('link', { name: 'Sign in with a passkey' }).getAttribute('href')).toContain('idp=passkey')
    expect(screen.getByRole('link', { name: 'Continue with Microsoft' }).getAttribute('href')).toContain('idp=microsoft')
  })

  it('shows an error that the callback passed back', async () => {
    renderApp('/signin?error=access_denied')
    expect(await screen.findByRole('alert')).toHaveTextContent('Sign-in was cancelled.')
  })

  it('never puts an absolute URL in return_to', () => {
    expect(safeReturnTo('https://evil.example/')).toBe('/')
    expect(safeReturnTo('//evil.example/')).toBe('/')
    expect(safeReturnTo('/\\evil.example')).toBe('/')
    expect(safeReturnTo('/t/x/files')).toBe('/t/x/files')
    expect(loginUrl('//evil.example')).toBe('/v1/auth/login?return_to=%2F')
  })

  it('opens the files of the home tenant after sign-in', async () => {
    const { router } = renderApp('/')
    expect(await screen.findByRole('heading', { name: 'Files' })).toBeInTheDocument()
    expect(router.state.location.pathname).toBe(`${personal}/files`)
    expect(await screen.findByRole('link', { name: 'Documents' })).toBeInTheDocument()
  })

  it('signs out with logout and follows the logout URL', async () => {
    const seen = recordRequests()
    const { user, assign } = renderApp(`${personal}/files`)
    const nav = await screen.findByRole('navigation', { name: 'Main' })
    await user.click(within(nav).getByRole('button', { name: 'Sign out' }))
    await vi.waitFor(() => expect(assign).toHaveBeenCalledWith('/signin?signed_out=1'))
    expect(seen.some((r) => r.method === 'POST' && r.path === '/v1/auth/logout')).toBe(true)
  })

  it('goes to sign-in when a later call answers 401', async () => {
    const { user, router } = renderApp(`${personal}/files`)
    await screen.findByRole('link', { name: 'Documents' })
    db.signedIn = false
    await user.click(screen.getByRole('link', { name: 'Documents' }))
    await screen.findByRole('heading', { name: 'Sign in to Ruum' })
    expect(router.state.location.pathname).toBe('/signin')
  })
})
