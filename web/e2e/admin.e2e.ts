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
  await ann.getByRole('button', { name: /\(change space\)$/ }).click()
  await ann.getByRole('menuitem', { name: 'Start a team space...' }).click()
  const start = ann.getByRole('dialog', { name: 'Start a team space' })
  await start.getByLabel('Team name').fill(team)
  await start.getByRole('button', { name: 'Create team space' }).click()
  await expect(toast(ann, `${team} is ready. Invite the people you work with.`)).toBeVisible()
  await expect(ann.getByRole('heading', { name: `Space settings: ${team}`, level: 1 })).toBeVisible()
  await expect(ann.getByRole('button', { name: `${team} (change space)` })).toBeVisible()
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Members' }).click()

  // Members: Ann alone, as the owner; she cannot change her own role.
  const members = ann.getByRole('region', { name: 'Members' })
  const annRow = members.getByRole('row').filter({ hasText: '(you)' })
  await expect(annRow).toContainText(users.ann.displayName)
  await expect(annRow).toContainText(users.ann.email)
  // The role is text with the reason next to it (journey J6): no menu for her own row.
  await expect(annRow).toContainText('Owner')
  await expect(annRow).toContainText('A space needs at least one owner.')
  await expect(annRow.getByRole('button', { name: /Role of/ })).toHaveCount(0)

  // An invitation to Cara as a member; the mail reaches Mailpit with the accept link.
  const link = await invite(ann, users.cara.email, 'Member')
  expect(new URL(link).origin).toBe(new URL(ann.url()).origin)

  // Cara opens the link of the mail without a session: sign-in, then back on the accept page.
  const caraContext = await browser.newContext()
  const cara = await caraContext.newPage()
  const url = new URL(link)
  // The token is in the fragment: it waits in the tab's session storage while Cara signs in. The
  // sign-in page already names the space and the masked address (`previewInvitation`, C-9).
  await cara.goto(url.pathname + url.search + url.hash)
  await expect(cara.getByText(`Sign in, or make a free account, to join ${team}. The invitation was sent to `)).toBeVisible()
  await expect(cara.getByText(users.cara.email)).toHaveCount(0)
  await signIn(cara, users.cara, url.pathname + url.search + url.hash, { shell: false })
  // The accept page names the space, the inviter and the role (C-9).
  await expect(cara.getByRole('heading', { name: `${users.ann.displayName} invited you to ${team}`, level: 1 })).toBeVisible()
  await expect(cara.getByText(`As a member, you can do everything with the files in ${team}, also move, delete and restore them.`)).toBeVisible()
  await cara.getByRole('button', { name: `Join ${team}` }).click()
  await expect(toast(cara, `You've joined ${team}.`)).toBeVisible()
  await expect(cara.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  await expect(cara.getByRole('button', { name: `${team} (change space)` })).toBeVisible()
  await expect(cara.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' })).toHaveCount(0)

  // Ann sees Cara among the members, and no open invitation; she makes Cara an admin.
  await ann.reload()
  await expect(ann.getByText('No open invitations.')).toBeVisible()
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Members' }).click()
  const caraRow = members.getByRole('row').filter({ hasText: users.cara.email })
  await expect(caraRow).toContainText(users.cara.displayName)
  // A member has a content role, default Can manage (decision 0003-1).
  await expect(caraRow.getByRole('button', { name: `Can manage what ${users.cara.displayName} can do` })).toBeVisible()
  await caraRow.getByRole('button', { name: `Member Role of ${users.cara.displayName}` }).click()
  await ann.getByRole('menu').getByRole('menuitemradio', { name: /^Admin/ }).click()
  await expect(toast(ann, `${users.cara.displayName} is now admin.`)).toBeVisible()
  await expect(caraRow.getByRole('button', { name: `Admin Role of ${users.cara.displayName}` })).toBeVisible()
  await expect(caraRow.getByRole('button', { name: /can do$/ })).toHaveCount(0)

  // Cara now has the admin pages of the team.
  await cara.reload()
  await expect(cara.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' })).toBeVisible()

  // The activity log records the invitation and the role change, in plain words.
  await ann.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Activity log' }).click()
  const log = ann.getByRole('region', { name: 'Activity log' })
  await expect(log).toContainText(/invited |accepted an invitation/)
  await expect(log).not.toContainText('invitation.created')

  await annContext.close()
  await caraContext.close()
})

async function invitePage(page: import('@playwright/test').Page) {
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Space settings' }).click()
  await page.getByRole('navigation', { name: 'Admin' }).getByRole('link', { name: 'Invitations' }).click()
}
