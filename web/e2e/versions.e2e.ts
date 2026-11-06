// Version history as Ann: a named first version, a new version, the compare view, and a restore
// of the first one; and the warning when a new version would replace her own recent one.
import fs from 'node:fs'
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { testFile, toast, uniqueName } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

test('version history: label, new version, diff and restore', async ({ page }, info) => {
  const { run } = readRun()
  const folder = uniqueName('Versions', run)
  const v1 = 'apples\nbananas\ncherries\n'
  const v2 = 'apples\nblueberries\ncherries\ndates\n'

  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name: folder, exact: true }).click()
  await expect(page.getByRole('heading', { name: folder, level: 1 })).toBeVisible()

  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('v1'), 'plan.txt', v1))
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()
  await page.getByRole('button', { name: 'Details of plan.txt' }).click()

  // Saves by the same person within 10 minutes merge into one version (design 5.3); a name keeps
  // the first version on its own.
  await page.getByRole('button', { name: 'Versions', exact: true }).click()
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  await expect(panel.getByRole('listitem')).toHaveCount(1)
  await expect(panel.getByText('This is the only version')).toBeVisible()
  await panel.getByRole('button', { name: /^Keep forever: name the version of/ }).click()
  const nameDialog = page.getByRole('dialog', { name: 'Name this version' })
  await expect(nameDialog).toContainText('Named versions are kept forever')
  await nameDialog.getByLabel('Version name').fill('first')
  await nameDialog.getByRole('button', { name: 'Save name' }).click()
  await expect(toast(page, 'Named the version first.')).toBeVisible()
  await expect(panel.getByText('first', { exact: true })).toBeVisible()
  await panel.getByRole('button', { name: 'Back to details' }).click()

  // A new version through "Upload new version" (target new_version with the head as base).
  await page.getByLabel('New version of plan.txt').setInputFiles(testFile(info.outputPath('v2'), 'plan.txt', v2))
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()
  await page.getByRole('button', { name: 'Versions', exact: true }).click()
  await expect(panel.getByRole('listitem')).toHaveCount(2)
  const current = panel.getByRole('listitem').filter({ hasText: 'Current' })
  await expect(current).toHaveCount(1)
  await expect(current).not.toContainText('first')
  const first = panel.getByRole('listitem').filter({ hasText: 'first' })

  // The first version against the current one, in a wide dialog: the text plugin, line changes.
  await first.getByRole('button', { name: /^Compare version of/ }).click()
  const compare = page.getByRole('dialog', { name: 'Compare versions' })
  const changes = compare.getByRole('group', { name: 'Changes' })
  await expect(changes).toBeVisible()
  await expect(changes).toContainText('Compared as text.')
  await expect(compare).not.toContainText('text@')
  await expect(changes.locator('ins', { hasText: 'blueberries' }).first()).toBeVisible()
  await expect(changes.locator('del', { hasText: 'bananas' }).first()).toBeVisible()
  await expect(changes.locator('ins', { hasText: 'dates' }).first()).toBeVisible()
  await compare.getByRole('button', { name: 'Close', exact: true }).last().click()

  // Restore the first version: a new current version with its content; nothing is lost.
  await first.getByRole('button', { name: /^Restore version of/ }).click()
  await page.getByRole('dialog', { name: /^Restore the version of / }).getByRole('button', { name: 'Restore' }).click()
  await expect(toast(page, /^Restored\. The version of .* is current again\.$/)).toBeVisible()
  await expect(panel.getByRole('listitem')).toHaveCount(3)
  const restored = panel.getByRole('listitem').filter({ hasText: 'Current' })
  await expect(restored).toContainText(/Restored from /)

  const [dl] = await Promise.all([page.waitForEvent('download'), restored.getByRole('button', { name: /^Download version of/ }).click()])
  const file = info.outputPath('restored.txt')
  await dl.saveAs(file)
  expect(fs.readFileSync(file, 'utf8')).toBe(v1)
})

test('"Upload new version" warns before it replaces your own version of the last 10 minutes', async ({ page }, info) => {
  const folder = uniqueName('Coalesce', readRun().run)
  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name: folder, exact: true }).click()
  await expect(page.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('a'), 'plan.txt', 'one\n'))
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()
  await page.getByRole('button', { name: 'Details of plan.txt' }).click()

  // The same person uploads again a minute later: Ruum would join it into the version before.
  await page.getByLabel('New version of plan.txt').setInputFiles(testFile(info.outputPath('b'), 'plan.txt', 'one\ntwo\n'))
  const keep = page.getByRole('dialog', { name: 'Keep your last change as its own version?' })
  await expect(keep).toContainText('this upload replaces that version')
  await keep.getByLabel('Name the current version to keep it (optional)').fill('before two')
  await keep.getByRole('button', { name: 'Keep it and upload' }).click()
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()
  await page.getByRole('button', { name: 'Versions', exact: true }).click()
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  // Both versions are there: the named one kept the old content.
  await expect(panel.getByRole('listitem')).toHaveCount(2)
  await expect(panel.getByText('before two', { exact: true })).toBeVisible()

  // A conflict on upload asks once. The current version is Ann's own unnamed one from just now,
  // so "Save as a new version" (the default) names it first and says so: nothing is lost.
  await page.getByRole('button', { name: 'Back to details' }).click()
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('c'), 'plan.txt', 'one\ntwo\nthree\n'))
  const conflict = page.getByRole('dialog', { name: 'plan.txt is already here' })
  await expect(conflict.getByRole('radio', { name: 'Save as a new version' })).toBeChecked()
  await expect(conflict).toContainText('Ruum keeps that version under this name, so nothing is lost.')
  await conflict.getByRole('radio', { name: 'Keep both, as two separate files' }).check()
  await conflict.getByRole('button', { name: 'Continue' }).click()
  await expect(page.getByRole('region', { name: 'Uploads' })).toContainText('Done, saved as plan (2).txt')
  await expect(page.getByRole('button', { name: 'plan (2).txt', exact: true })).toBeVisible()
})

test('a name conflict on upload does not silently replace your own version of the last 10 minutes', async ({ page }, info) => {
  const folder = uniqueName('Conflict', readRun().run)
  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name: folder, exact: true }).click()
  await expect(page.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('a'), 'plan.txt', 'plan v1'))
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()

  // The same name again, a moment later. Enter takes the default: a new version, and the
  // earlier one is kept under a name, so both versions are there.
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('b'), 'plan.txt', 'plan v2 changed'))
  let conflict = page.getByRole('dialog', { name: 'plan.txt is already here' })
  await expect(conflict).toContainText('You changed plan.txt')
  await expect(conflict.getByLabel('Name of the earlier version')).toHaveValue(/^Before the upload of \d\d:\d\d$/)
  await page.keyboard.press('Enter')
  await expect(toast(page, /^Uploaded plan\.txt\.$/)).toBeVisible()
  await page.getByRole('button', { name: 'Details of plan.txt' }).click()
  await page.getByRole('button', { name: 'Versions', exact: true }).click()
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  await expect(panel.getByRole('listitem')).toHaveCount(2)
  await expect(panel.getByText(/^Before the upload of /)).toBeVisible()
  await page.getByRole('button', { name: 'Back to details' }).click()

  // Without a name the dialog says the upload replaces the version of the last minutes.
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('c'), 'plan.txt', 'plan v3'))
  conflict = page.getByRole('dialog', { name: 'plan.txt is already here' })
  await conflict.getByLabel('Name of the earlier version').fill('')
  await expect(conflict.getByRole('note')).toContainText('Without a name, the new upload replaces your version from')
  await conflict.getByRole('button', { name: 'Continue' }).click()
  await expect(toast(page, /^Uploaded plan\.txt\. It replaced your version from /)).toBeVisible()
  await page.getByRole('button', { name: 'Versions', exact: true }).click()
  await expect(panel.getByRole('listitem')).toHaveCount(2)
})
