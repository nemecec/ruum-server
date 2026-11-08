// The UI of the step 1 follow-ups (decision 0003; ruum-server-wt/panel/ui-followups.md) and the
// design system fixes of the UX panel that are not covered by the screen flow tests.
import { act, screen, waitFor, within } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { afterEach, describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { server } from '../../mocks/node'
import { resetEmailShareChecks } from '../../pages/files/EmailShareCheck'
import { ids, personal, renderApp, team } from '../render'
import { recordRequests } from '../requests'

const notFound = () => HttpResponse.json({ error: { code: 'not_found', message: 'Node not found.' } }, { status: 404 })

afterEach(() => {
  resetEmailShareChecks()
  sessionStorage.clear()
})

describe('item of a share email (decision 0003-2, 0003-3)', () => {
  it('checks the email grants once on 404, then opens the item', async () => {
    const seen = recordRequests()
    let bound = false
    server.use(
      http.get('*/v1/nodes/:id', ({ params }) => (params.id === ids.teamSpecs && !bound ? notFound() : undefined)),
      http.get('*/v1/shared-with-me', ({ request }) => {
        if (new URL(request.url).searchParams.get('check_email_grants') === 'true') bound = true
        return undefined
      }),
    )
    renderApp(`${team}/item/${ids.teamSpecs}`)
    expect(await screen.findByRole('heading', { name: 'engine-specs.txt' })).toBeInTheDocument()
    const checks = seen.filter((r) => r.path.startsWith('/v1/shared-with-me') && r.path.includes('check_email_grants=true'))
    expect(checks).toHaveLength(1)
  })

  it('says "another address" when the share is still not found, and never names the item or the sharer', async () => {
    server.use(http.get('*/v1/nodes/:id', ({ params }) => (params.id === ids.teamSpecs ? notFound() : undefined)))
    const { user, assign } = renderApp(`${team}/item/${ids.teamSpecs}`)
    expect(await screen.findByText("You can't open this with this account")).toBeInTheDocument()
    expect(
      screen.getByText(
        "You can't open this with the account you're signed in with. If this was shared with another email address, sign in with that address, or ask the person who shared it.",
      ),
    ).toBeInTheDocument()
    expect(screen.getByText(/New to Ruum\? Create an account with the address the email was sent to/)).toBeInTheDocument()
    expect(screen.queryByText(/engine-specs/)).not.toBeInTheDocument()
    expect(within(screen.getByRole('main')).getByRole('link', { name: 'Shared with me' })).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Sign in with another account' }))
    await waitFor(() => expect(assign).toHaveBeenCalled())
  })

  it('asks to verify the email address first while it is not verified', async () => {
    db.user.email_verified = false
    server.use(http.get('*/v1/nodes/:id', ({ params }) => (params.id === ids.teamSpecs ? notFound() : undefined)))
    renderApp(`${team}/item/${ids.teamSpecs}`)
    expect(await screen.findByText('Verify your email address first. Then open the link in the email again.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'I verified it, check again' })).toBeInTheDocument()
  })
})

describe('invitation link (decision 0003-3)', () => {
  it('reads the token from the fragment', async () => {
    const seen = recordRequests()
    window.history.replaceState(null, '', '/invitations/accept#token=demo-invite')
    const { user } = renderApp('/invitations/accept')
    await user.click(await screen.findByRole('button', { name: 'Join Lovelace Lab' }))
    // The preview and the accept both send the fragment's token in the body, never in a URL.
    expect(seen.find((r) => r.path === '/v1/invitations/preview')?.body).toEqual({ token: 'demo-invite' })
    await waitFor(() => expect(seen.find((r) => r.path === '/v1/invitations/accept')?.body).toEqual({ token: 'demo-invite' }))
    window.history.replaceState(null, '', '/')
  })

  it('keeps a fragment token over the sign-in, not in return_to', async () => {
    const { signInPath } = await import('../../app/auth')
    const path = signInPath({ pathname: '/invitations/accept', search: '', hash: '#token=secret-one' })
    expect(path).toBe('/signin?return_to=%2Finvitations%2Faccept')
    expect(sessionStorage.getItem('ruum.invitationToken')).toBe('secret-one')
  })
})

describe('pages without a space (decision 0003-7, user_required hints)', () => {
  it.each([
    ['/agents', `${personal}/agents`],
    ['/members', `${personal}/admin/members`],
    ['/audit', `${personal}/admin/audit`],
    ['/settings', `${personal}/admin/policy`],
    ['/account', `${personal}/account`],
  ])('%s goes to that page of the last used space', async (from, to) => {
    const { router } = renderApp(from)
    await waitFor(() => expect(router.state.location.pathname).toBe(to))
  })
})

describe('design system fixes (UX panel)', () => {
  it('keeps an error toast until it is closed, as an assertive announcement', async () => {
    server.use(
      http.post('*/v1/nodes/:id/restore', () => HttpResponse.json({ error: { code: 'internal', message: 'boom' } }, { status: 500 })),
    )
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('checkbox', { name: 'Select notes.md' }))
    await user.click(within(screen.getByRole('region', { name: 'Selection' })).getByRole('button', { name: 'Move to trash' }))
    const region = screen.getByRole('region', { name: 'Notifications' })
    await user.click(await within(region).findByRole('button', { name: 'Undo' }))
    const live = region.querySelector('[aria-live="assertive"]')!
    await waitFor(() => expect(live.textContent).toMatch(/went wrong|try again/i))
  })

  it('moves the focus to the newest toast with Alt+T', async () => {
    const { user } = renderApp(`${personal}/files`)
    await user.click(await screen.findByRole('checkbox', { name: 'Select notes.md' }))
    await user.click(within(screen.getByRole('region', { name: 'Selection' })).getByRole('button', { name: 'Move to trash' }))
    await screen.findByText('Moved notes.md to the trash.')
    await user.keyboard('{Alt>}t{/Alt}')
    expect(document.activeElement).toHaveTextContent('Moved notes.md to the trash.')
  })

  it('skips disabled items of a menu with the arrow keys, but lets them take the focus to be heard', async () => {
    const { user } = renderApp(`${team}/admin/members`)
    // An admin may not make an owner: the menu has no Owner item; Guest, Member, Admin remain.
    await user.click(await screen.findByRole('button', { name: 'Member Role of Grace Hopper' }))
    const menu = screen.getByRole('menu')
    // The menu opens on the checked item.
    expect(within(menu).getByRole('menuitemradio', { name: /^Member/ })).toHaveFocus()
    await user.keyboard('{ArrowDown}')
    expect(within(menu).getByRole('menuitemradio', { name: /^Admin/ })).toHaveFocus()
    await user.keyboard('{Escape}')
    expect(screen.queryByRole('menu')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Member Role of Grace Hopper' })).toHaveFocus()
  })

  it('shows nothing for the first 300 ms of a load, then a status', async () => {
    let release: () => void = () => {}
    server.use(
      http.get('*/v1/trash', async () => {
        await new Promise<void>((r) => (release = r))
        return undefined
      }),
    )
    renderApp(`${personal}/trash`)
    await screen.findByRole('heading', { name: 'Trash' })
    expect(screen.queryByText('Loading...')).not.toBeInTheDocument()
    expect(await screen.findByText('Loading...', {}, { timeout: 2000 })).toBeInTheDocument()
    await act(async () => release())
  })

  it('offers "Try again" for an error that a new try can help', async () => {
    let fail = true
    server.use(
      http.get('*/v1/trash', () =>
        fail ? HttpResponse.json({ error: { code: 'unavailable', message: 'down' } }, { status: 503 }) : undefined,
      ),
    )
    const { user } = renderApp(`${personal}/trash`)
    const retry = await screen.findByRole('button', { name: 'Try again' })
    fail = false
    await user.click(retry)
    expect(await screen.findByRole('table', { name: 'Items in the trash' })).toBeInTheDocument()
  })

  it('names the page in the document title', async () => {
    renderApp(`${team}/trash`)
    await screen.findByRole('heading', { name: 'Trash' })
    await waitFor(() => expect(document.title).toBe('Trash - Analytical Engines - Ruum'))
  })

  it('says how many days items stay in the trash, from the plan', async () => {
    renderApp(`${team}/trash`)
    expect(await screen.findByText(/Items stay in the trash for 30 days\./)).toBeInTheDocument()
  })
})
