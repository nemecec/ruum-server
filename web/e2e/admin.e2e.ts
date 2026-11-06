// Space settings as Ann: a new team space, its members, an invitation through Mailpit and the
// accept page, and a role change.
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { invite, signIn, toast, uniqueName } from './support/app.ts'

test('a team: members, an invitation by mail, a new member and a role change', async ({ browser }) => {
  const { run, users } = readRun()
  const team = uniqueName('Team', run)

  const annContext = await browser.newContext({ storageState: storageStatePath('ann') })
  const ann = await annContext.newPage()
  await ann.goto('/')
  await expect(ann.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  // A personal space takes guests only, and points to a team space.
  await invitePage(ann)
  const form = ann.getByRole('form', { name: 'Invite people' })
  await expect(form.getByLabel('Role').locator('option')).toHaveText(['Guest'])
  await expect(form).toContainText('is a personal space, just for you')
  // "Start a team space" in the space switcher (journeys J6 step 1).
  await ann.getByRole('combobox', { name: 'Space' }).selectOption({ label: 'Start a team space...' })
  const start = ann.getByRole('dialog', { name: 'Start a team space' })
  await start.getByLabel('Team name').fill(team)
  await start.getByRole('button', { name: 'Create team space' }).click()
  await expect(toast(ann, `${team} is ready. Invite the people you work with.`)).toBeVisible()
  await expect(ann.getByRole('heading', { name: `Space settings: ${team}`, level: 1 })).toBeVisible()
  await expect(ann.getByRole('combobox', { name: 'Space' })).toHaveValue(/[0-9a-f-]{36}/)
  await expect(ann.getByRole('combobox', { name: 'Space' }).locator('option:checked')).toHaveText(`${team} (owner)`)
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Members' }).click()

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

  // Cara opens the link of the mail without a session: sign-in, then back on the accept page.
  const caraContext = await browser.newContext()
  const cara = await caraContext.newPage()
  const url = new URL(link)
  await signIn(cara, users.cara, url.pathname + url.search, { shell: false })
  await expect(cara.getByRole('heading', { name: "You're invited to a space in Ruum" })).toBeVisible()
  await cara.getByRole('button', { name: 'Join the space' }).click()
  await expect(toast(cara, `You've joined ${team}.`)).toBeVisible()
  await expect(cara.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  await expect(cara.getByRole('combobox', { name: 'Space' }).locator('option:checked')).toHaveText(`${team} (member)`)
  await expect(cara.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' })).toHaveCount(0)

  // Ann sees Cara among the members, and no open invitation; she makes Cara an admin.
  await ann.reload()
  await expect(ann.getByText('No open invitations.')).toBeVisible()
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Members' }).click()
  const caraRow = members.getByRole('row').filter({ hasText: users.cara.email })
  await expect(caraRow).toContainText(users.cara.displayName)
  await expect(caraRow.getByRole('combobox')).toHaveValue('member')
  await caraRow.getByRole('combobox').selectOption('admin')
  await expect(toast(ann, `${users.cara.displayName} is now admin.`)).toBeVisible()
  await expect(caraRow.getByRole('combobox')).toHaveValue('admin')

  // Cara now has the admin pages of the team.
  await cara.reload()
  await expect(cara.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' })).toBeVisible()

  // The activity log records the invitation and the role change, in plain words.
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Activity log' }).click()
  const log = ann.getByRole('region', { name: 'Activity log' })
  await expect(log).toContainText(/invited someone|accepted an invitation/)
  await expect(log).not.toContainText('invitation.created')

  await annContext.close()
  await caraContext.close()
})

async function invitePage(page: import('@playwright/test').Page) {
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' }).click()
  await page.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Invitations' }).click()
}
