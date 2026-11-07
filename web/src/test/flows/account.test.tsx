import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { dialog, personal, renderApp } from '../render'

describe('account page', () => {
  it('shows the profile, sign-in methods and sessions', async () => {
    renderApp('/account')
    expect(await screen.findByRole('heading', { name: 'Account', level: 1 })).toBeInTheDocument()
    expect(screen.getByText('ada@example.com')).toBeInTheDocument()
    const methods = screen.getByRole('region', { name: 'Sign-in methods' })
    expect(await within(methods).findByText('Google')).toBeInTheDocument()
    expect(within(methods).getByText('Passkey')).toBeInTheDocument()
    const sessions = screen.getByRole('region', { name: 'Signed-in devices' })
    expect(await within(sessions).findByText('This device')).toBeInTheDocument()
  })

  it('ends another session', async () => {
    const { user } = renderApp(`${personal}/account`)
    await user.click(await screen.findByRole('button', { name: 'Sign out device Safari on iPhone' }))
    await user.click(within(await dialog('End this session?')).getByRole('button', { name: 'End session' }))
    expect(await screen.findByText('Session ended.')).toBeInTheDocument()
    expect(db.sessions).toHaveLength(1)
  })

  it('creates an app password and shows it once', async () => {
    const { user } = renderApp(`${personal}/account`)
    const section = await screen.findByRole('region', { name: 'App passwords' })
    await user.click(within(section).getByRole('button', { name: 'New app password' }))
    const d = await dialog('New app password')
    await user.type(within(d).getByRole('textbox', { name: 'Name' }), 'Explorer{Enter}')
    const secret = await within(d).findByRole('textbox', { name: 'App password' })
    expect((secret as HTMLInputElement).value).toMatch(/^ruum_app_[A-Za-z0-9_-]{43}$/)
    // The WebDAV host to mount (decision 0002-3), not the platform origin.
    const address = within(d).getByRole('textbox', { name: 'Server address' })
    expect((address as HTMLInputElement).value).toBe('https://dav.ruum.localhost/')
    await user.click(within(d).getByRole('button', { name: 'Done' }))
    expect(await within(section).findByText('Explorer')).toBeInTheDocument()
  })

  it('starts a team space and opens its invitations', async () => {
    const { user, router } = renderApp(`${personal}/account`)
    await user.click(await screen.findByRole('button', { name: 'Start a team space' }))
    const d = await dialog('Start a team space')
    await user.type(within(d).getByRole('textbox', { name: 'Team name' }), 'Lab')
    await user.click(within(d).getByRole('button', { name: 'Create team space' }))
    await waitFor(() => expect(router.state.location.pathname).toMatch(/\/admin\/invitations$/))
    expect(await screen.findByRole('heading', { name: 'Space settings: Lab' })).toBeInTheDocument()
  })
})
