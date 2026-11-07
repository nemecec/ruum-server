// Sharing: Ann shares a folder by email (decision 0003-2, 0003-3); Ruum emails the person, who
// opens the link of the mail and sees the folder with view access only. A link made in the share
// dialog opens for a visitor without an account.
import fs from 'node:fs'
import { expect, test, type Browser, type Page } from '@playwright/test'
import { readRun, storageStatePath, type TestUser } from './support/env.ts'
import { api, signIn, testFile, toast, uniqueName, waitForMail } from './support/app.ts'

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
  // The email grant binds when the person signs in with this address; "Notify people" is on by
  // default, and the share dialog never creates an invitation (decision 0003-3).
  await expect(form).toContainText('They sign in with this address to open it.')
  await expect(form.getByRole('checkbox', { name: 'Notify people' })).toBeChecked()
  await expect(form.getByRole('checkbox', { name: /invitation/ })).toHaveCount(0)
  await expect(form).not.toContainText('make a free')
  await expect(form.getByRole('button', { name: 'Use a link instead' })).toBeVisible()
  await form.getByLabel('Message (optional)').fill(`The plans, run ${run}.`)
  await form.getByRole('button', { name: 'Share' }).click()
  await expect(share.getByRole('status').filter({ hasText: `Shared with ${email}. Ruum emailed them.` })).toBeVisible()
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
  const row = page.getByRole('row').filter({ has: page.getByRole('link', { name: folder, exact: true }) })
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
  // The More menu has no write actions for a viewer.
  await details.getByRole('button', { name: 'More actions for note.txt' }).click()
  await expect(page.getByRole('menuitem', { name: 'Versions' })).toBeVisible()
  await expect(page.getByRole('menuitem', { name: 'Rename' })).toHaveCount(0)
  await expect(page.getByRole('menuitem', { name: 'Move to trash' })).toHaveCount(0)
  await page.keyboard.press('Escape')
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

/** The link of a share email: the item page of the web UI, never a token (decision 0003-3). */
function shareLink(text: string): string {
  const link = /https?:\/\/\S+\/t\/[0-9a-f-]{36}\/(?:files|item)\/[0-9a-f-]{36}/.exec(text)?.[0]
  expect(link, text).toBeTruthy()
  expect(link).not.toContain('token')
  return link!
}

test('Ann shares a folder with Ben, who gets a mail, opens its link and sees the folder read-only', async ({ browser }, info) => {
  const { users } = readRun()
  const { context, ann, folder, folderId, tenant, note, share } = await annSharesFolder(browser, users.ben.email, info.outputPath())

  // Ben already has an account. The share mail names the item and Ann's message, and links to
  // the folder; opening it with his session binds the email grant, and he joins as a guest.
  const mail = await waitForMail(users.ben.email, 20_000, folder)
  expect(mail.text).toContain('The plans, run')
  const benContext = await browser.newContext({ storageState: storageStatePath('ben') })
  const ben = await benContext.newPage()
  await ben.goto(shareLink(mail.text))
  await expect(ben.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await ben.reload()
  // Ann's space is now in Ben's space list, with his role there.
  await ben.getByRole('button', { name: /\(change space\)$/ }).click()
  await expect(ben.getByRole('menuitemradio', { name: `${users.ann.displayName}'s space Your role: Guest` })).toHaveCount(1)
  await ben.keyboard.press('Escape')
  await seesFolderReadOnly(ben, users.ann, folder, note, info.outputPath('ben-note.txt'))

  // Ann removes Ben's access; his next look at the folder fails.
  await ann.goto(`/t/${tenant}/files`)
  await ann.getByRole('button', { name: `Details of ${folder}`, exact: true }).click()
  await ann.getByRole('button', { name: 'Share', exact: true }).click()
  // The bind turned the email grant into a grant to Ben's user: the row names him, never his ID.
  const benId = (await api<{ user: { id: string } }>(ben, 'GET', '/v1/me')).body.user.id
  const people = share.getByRole('region', { name: 'People with access' })
  await expect(people).toContainText(users.ben.displayName)
  await expect(people).not.toContainText(benId)
  await expect(people).not.toContainText("can't open it")
  // The row's role menu ends with "Remove access" (design-system 4.8).
  await share.getByRole('button', { name: `Can view Access of ${users.ben.displayName}` }).click()
  await ann.getByRole('menuitem', { name: 'Remove access' }).click()
  await expect(share.getByRole('status').filter({ hasText: `Removed access for ${users.ben.displayName}.` })).toBeVisible()
  await expect.poll(async () => (await api(ben, 'GET', `/v1/nodes/${folderId}`, { tenant })).status, { timeout: 15_000 }).toBe(404)

  // Ben is a guest of the space now. A new share to his email goes to his account at once (by
  // user ID, decision 0003-2 rule a): the row names him, never "Invited, not joined yet".
  const form = share.getByRole('form', { name: 'Add people' })
  await form.getByLabel('Email', { exact: true }).fill(users.ben.email)
  await expect(form).toContainText(`${users.ben.displayName} is in`)
  await form.getByRole('button', { name: 'Share' }).click()
  // A second mail for the same item within 10 minutes is not sent, and the dialog says so.
  await expect(
    share.getByRole('status').filter({ hasText: `Shared with ${users.ben.displayName}. Ruum did not email them this time; they find it under Shared with me.` }),
  ).toBeVisible()
  await expect(people).toContainText(users.ben.displayName)
  await expect(people).not.toContainText('Invited, not joined yet')
  await expect.poll(async () => (await api(ben, 'GET', `/v1/nodes/${folderId}`, { tenant })).status, { timeout: 15_000 }).toBe(200)

  // Shared with me names the space of the folder from the item itself (C-10).
  const shared = (await api<{ items: { node: { id: string }; tenant_name: string }[] }>(ben, 'GET', '/v1/shared-with-me')).body
  expect(shared.items.find((i) => i.node.id === folderId)?.tenant_name).toBe(`${users.ann.displayName}'s space`)
  await ben.goto(`/t/${tenant}/shared`)
  const row = ben.getByRole('row').filter({ has: ben.getByRole('link', { name: folder, exact: true }) })
  await expect(row).toContainText(`${users.ann.displayName}'s space`)

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

// The email grant binds at sign-in (decision 0003-2, rule b): Cara signs in with the address
// the share went to and opens the folder read-only.
test('a person signs in after a share by email and opens the folder read-only', async ({ browser }, info) => {
  const { users } = readRun()
  const { context, folder, note } = await annSharesFolder(browser, users.cara.email, info.outputPath())
  const caraContext = await browser.newContext()
  const cara = await caraContext.newPage()
  await signIn(cara, users.cara)
  await seesFolderReadOnly(cara, users.ann, folder, note, info.outputPath('cara-note.txt'))
  await context.close()
  await caraContext.close()
})

// A person with another account opens the link of a share email: the page says so and names
// neither the item nor the sharer (decision 0003-3, reviewer 2 #1).
test('a person signed in with another address sees the "another address" state', async ({ browser }, info) => {
  const { run, users } = readRun()
  const elsewhere = `someone-else-${run}@example.com`
  const { context, folder } = await annSharesFolder(browser, elsewhere, info.outputPath())
  const mail = await waitForMail(elsewhere, 20_000, folder)
  const benContext = await browser.newContext({ storageState: storageStatePath('ben') })
  const ben = await benContext.newPage()
  await ben.goto(shareLink(mail.text))
  await expect(ben.getByText("You can't open this with this account")).toBeVisible()
  await expect(ben.getByText(/If this was shared with another email address, sign in with that address/)).toBeVisible()
  await expect(ben.getByRole('main')).not.toContainText(folder)
  await expect(ben.getByRole('main')).not.toContainText(users.ann.displayName)
  await expect(ben.getByRole('button', { name: 'Sign in with another account' })).toBeVisible()
  await context.close()
  await benContext.close()
})
