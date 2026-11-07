// Version history as Ann: a named first version, a new version, the compare view, and a restore
// of the first one; and new versions from the web UI that never join her own recent one (C-8).
import fs from 'node:fs'
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { testFile, toast, uniqueName, menuAction } from './support/app.ts'

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
  await menuAction(page, 'More actions for plan.txt', 'Versions')
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  await expect(panel.getByRole('listitem')).toHaveCount(1)
  await expect(panel.getByText('This is the only version')).toBeVisible()
  await panel.getByRole('button', { name: /^More actions for the version of/ }).first().click()
  await page.getByRole('menuitem', { name: 'Name this version' }).click()
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
  await menuAction(page, 'More actions for plan.txt', 'Versions')
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

  // Download is in the row's More menu (the main actions of a version are Restore and Compare).
  await restored.getByRole('button', { name: /^More actions for the version of/ }).click()
  const [dl] = await Promise.all([page.waitForEvent('download'), page.getByRole('menu').getByRole('menuitem', { name: 'Download', exact: true }).click()])
  const file = info.outputPath('restored.txt')
  await dl.saveAs(file)
  expect(fs.readFileSync(file, 'utf8')).toBe(v1)
})

test('"Upload new version" always makes a new version, also right after your own (C-8)', async ({ page }, info) => {
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

  // The same person uploads again a moment later. The upload sends `coalesce: false`, so the
  // server makes a new version instead of joining it into the one before; nothing asks first.
  const created = page.waitForRequest((r) => r.method() === 'POST' && new URL(r.url()).pathname === '/v1/uploads')
  await page.getByLabel('New version of plan.txt').setInputFiles(testFile(info.outputPath('b'), 'plan.txt', 'one\ntwo\n'))
  expect((await created).postDataJSON()).toMatchObject({ target: 'new_version', coalesce: false })
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await menuAction(page, 'More actions for plan.txt', 'Versions')
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  // Both versions are there: the first one kept the old content.
  await expect(panel.getByRole('listitem')).toHaveCount(2)
  const older = panel.getByRole('listitem').filter({ hasNotText: 'Current' })
  await older.getByRole('button', { name: /^More actions for the version of/ }).click()
  const [dl] = await Promise.all([page.waitForEvent('download'), page.getByRole('menu').getByRole('menuitem', { name: 'Download', exact: true }).click()])
  const file = info.outputPath('older.txt')
  await dl.saveAs(file)
  expect(fs.readFileSync(file, 'utf8')).toBe('one\n')

  // A conflict on upload asks once, without a name to keep: "Keep both" makes a second file.
  await page.getByRole('button', { name: 'Back to details' }).click()
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('c'), 'plan.txt', 'one\ntwo\nthree\n'))
  const conflict = page.getByRole('dialog', { name: 'plan.txt is already here' })
  await expect(conflict.getByRole('radio', { name: 'Save as a new version' })).toBeChecked()
  await expect(conflict).toContainText('The earlier version stays in Versions.')
  await expect(conflict.getByRole('textbox')).toHaveCount(0)
  await conflict.getByRole('radio', { name: 'Keep both, as two separate files' }).check()
  await conflict.getByRole('button', { name: 'Continue' }).click()
  await expect(page.getByRole('region', { name: 'Uploads' })).toContainText('Done, saved as plan (2).txt')
  await expect(page.getByRole('button', { name: 'plan (2).txt', exact: true })).toBeVisible()
})

test('a name conflict on upload never replaces your own version of the last 10 minutes (C-8)', async ({ page }, info) => {
  const folder = uniqueName('Conflict', readRun().run)
  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name: folder, exact: true }).click()
  await expect(page.getByRole('heading', { name: folder, level: 1 })).toBeVisible()
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('a'), 'plan.txt', 'plan v1'))
  await expect(toast(page, 'Uploaded plan.txt.')).toBeVisible()
  // Like a person, upload the next one when plan.txt is in the list. (The product asks also when
  // the list is a moment old: the server answers name_taken; the Vitest flows cover that race.)
  await expect(page.getByRole('button', { name: 'plan.txt', exact: true })).toBeVisible()

  // The same name again, a moment later. "Save as a new version" (the default) makes a version
  // of its own: both versions are there, and no version is renamed to keep it.
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('b'), 'plan.txt', 'plan v2 changed'))
  let conflict = page.getByRole('dialog', { name: 'plan.txt is already here' })
  await expect(conflict.getByRole('radio', { name: 'Save as a new version' })).toBeChecked()
  await conflict.getByRole('button', { name: 'Continue' }).click()
  await expect(toast(page, /^Uploaded plan\.txt\.$/)).toBeVisible()
  await page.getByRole('button', { name: 'Details of plan.txt' }).click()
  await menuAction(page, 'More actions for plan.txt', 'Versions')
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  await expect(panel.getByRole('listitem')).toHaveCount(2)
  await page.getByRole('button', { name: 'Back to details' }).click()

  // Once more: a third version.
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('c'), 'plan.txt', 'plan v3'))
  conflict = page.getByRole('dialog', { name: 'plan.txt is already here' })
  await conflict.getByRole('button', { name: 'Continue' }).click()
  await expect(toast(page, /^Uploaded plan\.txt\.$/)).toBeVisible()
  await menuAction(page, 'More actions for plan.txt', 'Versions')
  await expect(panel.getByRole('listitem')).toHaveCount(3)
})
