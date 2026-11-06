import { screen, waitFor, within } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { db } from '../../mocks/db'
import { renderApp } from '../render'
import { recordRequests } from '../requests'

describe('share link visitor (/s#token)', () => {
  it('opens a folder link without an account, browses it and downloads a file', async () => {
    db.signedIn = false
    const seen = recordRequests()
    const { user, download } = renderApp('/s#demo-folder')
    expect(await screen.findByRole('heading', { name: 'Documents', level: 1 })).toBeInTheDocument()
    expect(screen.getByText("You're viewing as anyone with the link. You can view.")).toBeInTheDocument()
    // The reduced shell: no navigation of a signed-in person.
    expect(screen.queryByRole('navigation', { name: 'Main' })).not.toBeInTheDocument()

    await user.click(await screen.findByRole('button', { name: 'Download plan.txt' }))
    await waitFor(() => expect(download).toHaveBeenCalledTimes(1))
    expect(download.mock.calls[0][1]).toBe('plan.txt')

    // The token goes in the body of resolveShareLink, never into a URL; later calls send the
    // access token as Bearer, without cookies.
    const resolve = seen.find((r) => r.path === '/v1/share-links/resolve')!
    expect(resolve.body).toEqual({ token: 'demo-folder' })
    expect(seen.some((r) => r.path.includes('demo-folder'))).toBe(false)
    const children = seen.find((r) => r.path.includes('/children'))!
    expect(children.headers.get('Authorization')).toMatch(/^Bearer /)
  })

  it('asks for the password of a link that has one', async () => {
    db.signedIn = false
    const { user } = renderApp('/s#demo-locked')
    expect(await screen.findByRole('heading', { name: 'This link has a password' })).toBeInTheDocument()
    const field = screen.getByLabelText('Password')
    await user.type(field, 'wrong{Enter}')
    expect(await screen.findByText("That password isn't right. Try again.")).toBeInTheDocument()
    await user.type(screen.getByLabelText('Password'), 'open sesame{Enter}')
    // A file link: its page with Download.
    expect(await screen.findByRole('heading', { name: 'report.txt', level: 1 })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Download' })).toBeInTheDocument()
  })

  it('says so when a link does not work any more', async () => {
    db.signedIn = false
    db.shareLinks.find((l) => l.id === '0190a000-0000-7000-8000-000000000802')!.revoked_at = '2026-11-02T00:00:00Z'
    renderApp('/s#demo-folder')
    expect(await screen.findByRole('heading', { name: "This link doesn't work any more" })).toBeInTheDocument()
    expect(screen.getByText('Ask the person who sent it for a new one.')).toBeInTheDocument()
  })

  it('opens subfolders inside the link and goes back with the breadcrumbs', async () => {
    db.signedIn = false
    const sub = { ...db.nodes.find((n) => n.name === 'Documents')!, id: crypto.randomUUID(), name: 'Drafts' }
    sub.parent_id = db.nodes.find((n) => n.name === 'Documents')!.id
    db.nodes.push(sub)
    const { user } = renderApp('/s#demo-folder')
    await user.click(await screen.findByRole('button', { name: 'Drafts' }))
    expect(await screen.findByRole('heading', { name: 'Drafts', level: 1 })).toBeInTheDocument()
    expect(screen.getByText('This folder is empty')).toBeInTheDocument()
    await user.click(within(screen.getByRole('navigation', { name: 'Breadcrumbs' })).getByRole('button', { name: 'Documents' }))
    expect(await screen.findByRole('heading', { name: 'Documents', level: 1 })).toBeInTheDocument()
  })
})
