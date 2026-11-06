// Sharing: Ann shares a folder by email; the other person sees it, with view access only. A link
// made in the share dialog opens for a visitor without an account.
import fs from 'node:fs'
import { expect, test, type Browser, type Page } from '@playwright/test'
import { readRun, storageStatePath, type TestUser } from './support/env.ts'
import { acceptInvitation, api, signIn, testFile, toast, uniqueName, waitForMail } from './support/app.ts'

/** Ann makes a folder with note.txt and shares the folder with `email` as "Can view". */
async function annSharesFolder(browser: Browser, email: string, outDir: string) {
  const { run } = readRun()
  const folder = uniqueName('Shared', run)
  const note = `a note from Ann, run ${run}\n`
  const context = await browser.newContext({ storageState: storageStatePath('ann') })
  const ann = await context.newPage()
  await ann.goto('/')
  await ann.getByRole('button', { name: 'New folder' }).first().click()
  await ann.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await ann.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await ann.getByRole('link', { name: folder, exact: true }).click()
  await expect(ann.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await ann.getByLabel('Upload files').setInputFiles(testFile(outDir, 'note.txt', note))
  await expect(toast(ann, 'Uploaded note.txt.')).toBeVisible()

  const [, tenant, folderId] = /\/t\/([0-9a-f-]{36})\/files\/([0-9a-f-]{36})/.exec(ann.url())!
  await ann.getByRole('navigation', { name: 'Breadcrumbs' }).getByRole('link').first().click()
  await ann.getByRole('button', { name: `Details of ${folder}`, exact: true }).click()
  await ann.getByRole('button', { name: 'Share', exact: true }).click()
  const share = ann.getByRole('dialog', { name: `Share ${folder}` })
  const form = share.getByRole('form', { name: 'Add people' })
  await form.getByLabel('Email', { exact: true }).fill(email)
  await expect(form.getByLabel('Access')).toHaveValue('view')
  // The server sends no email for a share yet; the UI does not promise one.
  await expect(form.getByLabel('Send an email')).toBeDisabled()
  await expect(form).toContainText("Ruum can't send emails for shares yet")
  // The person is not in Ann's space: a share alone would not let them open the folder, so the
  // dialog says so and also invites them as a guest (on by default).
  await expect(form).toContainText(`${email} is not in`)
  await expect(form.getByRole('checkbox', { name: /^Also invite them to .* as a guest$/ })).toBeChecked()
  await form.getByRole('button', { name: 'Share' }).click()
  await expect(share.getByRole('status').filter({ hasText: `Shared with ${email}. Ruum sent them an invitation to join` })).toBeVisible()
  const people = share.getByRole('region', { name: 'People with access' })
  await expect(people).toContainText(email)
  await expect(people).toContainText('Invited, not joined yet')
  // The owner is the first row.
  await expect(people.getByRole('listitem').first()).toContainText('Owner')
  await share.getByRole('button', { name: 'Close' }).click()
  await expect(share).toHaveCount(0)
  return { context, ann, folder, folderId, tenant, note, share }
}

/** The other person finds the folder under "Shared with me" and gets view access only. */
async function seesFolderReadOnly(page: Page, owner: TestUser, folder: string, note: string, outFile: string) {
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Shared with me' }).click()
  await expect(page.getByRole('heading', { name: 'Shared with me', level: 1 })).toBeVisible()
  const row = page.getByRole('row').filter({ has: page.getByRole('rowheader', { name: folder }) })
  await expect(row).toContainText('Can view')
  // The space by its name, never "Another tenant" or an ID.
  await expect(row).toContainText(owner.displayName)
  await expect(row).not.toContainText(/[0-9a-f]{8}/)

  // The folder opens in Ann's tenant; the file is there, but nothing can be changed.
  await row.getByRole('link', { name: folder }).click()
  await expect(page.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await expect(page.getByRole('button', { name: 'note.txt', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'New folder' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Upload files', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Details of note.txt' }).click()
  const details = page.getByRole('complementary', { name: 'Details' })
  await expect(details.getByRole('definition').filter({ hasText: 'Can view' })).toBeVisible()
  await expect(details.getByRole('button', { name: 'Rename' })).toHaveCount(0)
  await expect(details.getByRole('button', { name: 'Move to trash' })).toHaveCount(0)
  const [dl] = await Promise.all([page.waitForEvent('download'), details.getByRole('button', { name: 'Download', exact: true }).click()])
  await dl.saveAs(outFile)
  expect(fs.readFileSync(outFile, 'utf8')).toBe(note)

  // The server refuses a write too, not only the UI.
  const folderId = /\/files\/([0-9a-f-]{36})/.exec(page.url())![1]
  const tenant = /\/t\/([0-9a-f-]{36})/.exec(page.url())![1]
  const write = await api(page, 'POST', '/v1/nodes', { tenant, body: { parent_id: folderId, name: 'not-mine', kind: 'folder' } })
  expect(write.status).toBe(403)
  return { folderId, tenant }
}

test('Ann shares a folder with Ben, who joins as a guest by invitation and sees it read-only', async ({ browser }, info) => {
  const { users } = readRun()
  const { context, ann, folder, folderId, tenant, note, share } = await annSharesFolder(browser, users.ben.email, info.outputPath())

  // Ben is a person outside Ann's space: he joins it as a guest through the invitation mail that
  // the share dialog sent, which also binds the email grant to him.
  const mail = await waitForMail(users.ben.email)
  expect(mail.text).toContain(`I shared ${folder} with you.`)
  const link = /https?:\/\/\S+\/invitations\/accept\?token=\S+/.exec(mail.text)?.[0]
  expect(link, mail.text).toBeTruthy()
  const benContext = await browser.newContext({ storageState: storageStatePath('ben') })
  const ben = await benContext.newPage()
  await ben.goto('/')
  await acceptInvitation(ben, link!)
  await ben.reload()
  // Ann's space is now in Ben's space list, with his role there.
  await expect(ben.getByRole('combobox', { name: 'Space' }).getByRole('option', { name: `${users.ann.displayName}'s space (guest)` })).toHaveCount(1)
  await seesFolderReadOnly(ben, users.ann, folder, note, info.outputPath('ben-note.txt'))

  // Ann removes Ben's access; his next look at the folder fails.
  await ann.goto(`/t/${tenant}/files`)
  await ann.getByRole('button', { name: `Details of ${folder}`, exact: true }).click()
  await ann.getByRole('button', { name: 'Share', exact: true }).click()
  // The accepted invitation turned the email grant into a grant to Ben's user. (listGrants gives
  // no name or email for a user grantee, so the dialog shows Ben's user ID; a small gap.)
  // The row names Ben (a guest of the space now, so the members list has his name), never his ID.
  const benId = (await api<{ user: { id: string } }>(ben, 'GET', '/v1/me')).body.user.id
  const people = share.getByRole('region', { name: 'People with access' })
  await expect(people).toContainText(users.ben.displayName)
  await expect(people).not.toContainText(benId)
  // Ben is a guest of the space now: no warning that he can't open it.
  await expect(people).not.toContainText("can't open it")
  await share.getByRole('button', { name: `Remove access for ${users.ben.displayName}` }).click()
  await expect(share.getByRole('status').filter({ hasText: `Removed access for ${users.ben.displayName}.` })).toBeVisible()
  await expect.poll(async () => (await api(ben, 'GET', `/v1/nodes/${folderId}`, { tenant })).status, { timeout: 15_000 }).toBe(404)

  await context.close()
  await benContext.close()
})

test('a link made in the share dialog opens for a visitor without an account', async ({ browser }, info) => {
  const { run } = readRun()
  const folder = uniqueName('Linked', run)
  const context = await browser.newContext({ storageState: storageStatePath('ann') })
  const ann = await context.newPage()
  await ann.goto('/')
  await ann.getByRole('button', { name: 'New folder' }).first().click()
  await ann.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await ann.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await ann.getByRole('link', { name: folder, exact: true }).click()
  await expect(ann.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await ann.getByLabel('Upload files').setInputFiles(testFile(info.outputPath(), 'site.txt', 'site plan\n'))
  await expect(toast(ann, 'Uploaded site.txt.')).toBeVisible()
  await ann.getByRole('navigation', { name: 'Breadcrumbs' }).getByRole('link').first().click()
  await ann.getByRole('button', { name: `Details of ${folder}`, exact: true }).click()
  await ann.getByRole('button', { name: 'Share', exact: true }).click()
  const share = ann.getByRole('dialog', { name: `Share ${folder}` })
  const linkForm = share.getByRole('form', { name: 'Create a link' })
  // The default end is visible.
  await expect(linkForm.getByLabel('Ends after (days)')).toHaveValue('7')
  await linkForm.getByRole('button', { name: 'Create link' }).click()
  const url = await share.getByLabel('New link', { exact: true }).inputValue()
  expect(url).toMatch(/\/s#/)
  await expect(share).toContainText('Anyone with the link: can view')

  // A visitor in a browser without a session.
  const visitorContext = await browser.newContext()
  const visitor = await visitorContext.newPage()
  const requests: string[] = []
  visitor.on('request', (r) => requests.push(r.url()))
  await visitor.goto(url)
  await expect(visitor.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await expect(visitor.getByText("You're viewing as anyone with the link. You can view.")).toBeVisible()
  const [dl] = await Promise.all([visitor.waitForEvent('download'), visitor.getByRole('button', { name: 'Download site.txt' }).click()])
  await dl.saveAs(info.outputPath('visitor-site.txt'))
  expect(fs.readFileSync(info.outputPath('visitor-site.txt'), 'utf8')).toBe('site plan\n')
  // The token is in the fragment; no request URL carries it.
  const token = new URL(url).hash.slice(1)
  expect(requests.some((r) => r.includes(token))).toBe(false)

  // Ann turns the link off; the visitor's next open says so.
  await share.getByRole('button', { name: 'Turn off the can view link' }).click()
  await ann.getByRole('dialog', { name: 'Turn off this link?' }).getByRole('button', { name: 'Turn off link' }).click()
  await expect(share).toContainText('Anyone with the link: off')
  await visitor.reload()
  await expect(visitor.getByRole('heading', { name: "This link doesn't work any more" })).toBeVisible()
  await visitorContext.close()
  await context.close()
})

// Known gap: an email grant that binds at a first sign-in does not make the person a guest of the
// tenant. "Shared with me" lists the folder ("Another tenant", not a link), but every read answers
// 404, so the person cannot open what was shared. The test above goes through an invitation.
test.fixme('a new person signs in after a share by email and opens the folder read-only', async ({ browser }, info) => {
  const { users } = readRun()
  const { context, folder, note } = await annSharesFolder(browser, users.cara.email, info.outputPath())
  const caraContext = await browser.newContext()
  const cara = await caraContext.newPage()
  await signIn(cara, users.cara)
  await seesFolderReadOnly(cara, users.ann, folder, note, info.outputPath('cara-note.txt'))
  await context.close()
  await caraContext.close()
})

// Known server gap (crates/ruum-core/src/identity/postgres.rs, sharing/postgres.rs): an email grant
// binds only when the user signs in for the first time (a known identity returns before the
// binding), and addGrant sends no invitation mail with a token, although the contract of addGrant
// says both. So a person who already has a Ruum account never gets a folder shared by email, and
// the web UI has no other way to share with them (AddPersonForm sends only email grantees).
test.fixme('Ann shares a folder with an existing user, who gets a mail and sees it read-only', async ({ browser }, info) => {
  const { users } = readRun()
  const { context, folder, note } = await annSharesFolder(browser, users.ben.email, info.outputPath())
  const mail = await waitForMail(users.ben.email)
  expect(mail.text).toContain(folder)
  const benContext = await browser.newContext()
  const ben = await benContext.newPage()
  await signIn(ben, users.ben)
  await seesFolderReadOnly(ben, users.ann, folder, note, info.outputPath('ben-note.txt'))
  await context.close()
  await benContext.close()
})
