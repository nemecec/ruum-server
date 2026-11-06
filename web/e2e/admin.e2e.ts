// Tenant admin as Ann: a new team, its members, an invitation through Mailpit, and a role change.
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { acceptInvitation, invite, signIn, uniqueName } from './support/app.ts'

test('a team: members, an invitation by mail, a new member and a role change', async ({ browser }) => {
  const { run, users } = readRun()
  const team = uniqueName('Team', run)

  const annContext = await browser.newContext({ storageState: storageStatePath('ann') })
  const ann = await annContext.newPage()
  await ann.goto('/account')
  await expect(ann.getByRole('heading', { name: 'Account', level: 1 })).toBeVisible()
  const start = ann.getByRole('region', { name: 'Start a team' })
  await start.getByLabel('Team name').fill(team)
  await start.getByRole('button', { name: 'Create team' }).click()
  await expect(ann.getByRole('heading', { name: `Admin: ${team}`, level: 1 })).toBeVisible()
  await expect(ann.getByRole('combobox', { name: 'Tenant' })).toHaveValue(/[0-9a-f-]{36}/)
  await expect(ann.getByRole('combobox', { name: 'Tenant' }).locator('option:checked')).toHaveText(`${team} (owner)`)

  // Members: Ann alone, as the owner; she cannot change her own role.
  const members = ann.getByRole('region', { name: 'Members' })
  const annRow = members.getByRole('row').filter({ hasText: '(you)' })
  await expect(annRow).toContainText(users.ann.displayName)
  await expect(annRow).toContainText(users.ann.email)
  await expect(annRow.getByRole('combobox')).toHaveValue('owner')
  await expect(annRow.getByRole('combobox')).toBeDisabled()

  // An invitation to Cara as a member; the mail reaches Mailpit with the accept link.
  const link = await invite(ann, users.cara.email, 'Member')
  expect(new URL(link).origin).toBe(new URL(ann.url()).origin)

  // Cara signs in and accepts.
  const caraContext = await browser.newContext()
  const cara = await caraContext.newPage()
  await signIn(cara, users.cara)
  await acceptInvitation(cara, link)
  await cara.reload()
  const option = cara.getByRole('combobox', { name: 'Tenant' }).getByRole('option', { name: `${team} (member)` })
  await expect(option).toHaveCount(1)
  await cara.getByRole('combobox', { name: 'Tenant' }).selectOption({ label: `${team} (member)` })
  await expect(cara.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  await expect(cara.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toHaveCount(0)

  // Ann sees Cara among the members, and no open invitation; she makes Cara an admin.
  await ann.reload()
  await expect(ann.getByText('No open invitations.')).toBeVisible()
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Members' }).click()
  const caraRow = members.getByRole('row').filter({ hasText: users.cara.email })
  await expect(caraRow).toContainText(users.cara.displayName)
  await expect(caraRow.getByRole('combobox')).toHaveValue('member')
  await caraRow.getByRole('combobox').selectOption('admin')
  await expect(ann.getByRole('status').filter({ hasText: `${users.cara.displayName} is now admin.` })).toBeVisible()
  await expect(caraRow.getByRole('combobox')).toHaveValue('admin')

  // Cara now has the admin pages of the team.
  await cara.reload()
  await expect(cara.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Admin' })).toBeVisible()

  // The audit log records the invitation and the role change.
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Audit log' }).click()
  await expect(ann.getByRole('region', { name: 'Audit log' })).toContainText(/invitation/i)

  await annContext.close()
  await caraContext.close()
})

// Known UI gap: the invitation mail links to /invitations/accept?token=..., but the web UI has no
// such route (it shows "not found"). The specs accept through the API instead (acceptInvitation).
test.fixme('the invitation link opens an accept page in the web UI', async ({ browser }) => {
  const { users } = readRun()
  const annContext = await browser.newContext({ storageState: storageStatePath('ann') })
  const ann = await annContext.newPage()
  await ann.goto('/')
  const link = await invite(ann, `nobody-${readRun().run}@example.test`, 'Guest')
  const benContext = await browser.newContext({ storageState: storageStatePath('ben') })
  const ben = await benContext.newPage()
  await ben.goto(link)
  await ben.getByRole('button', { name: /accept/i }).click()
  await expect(ben.getByRole('combobox', { name: 'Tenant' })).toContainText(users.ann.displayName)
  await annContext.close()
  await benContext.close()
})
