// Version history as Ann: a labelled first version, a new version, the diff between them, and a
// restore of the first one.
import fs from 'node:fs'
import { expect, test } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { testFile, uniqueName } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

test('version history: label, new version, diff and restore', async ({ page }, info) => {
  const { run } = readRun()
  const folder = uniqueName('Versions', run)
  const v1 = 'apples\nbananas\ncherries\n'
  const v2 = 'apples\nblueberries\ncherries\ndates\n'

  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(folder)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name: folder, exact: true }).click()
  // Wait for the folder: while it loads, the page still shows the previous folder's toolbar, and
  // an upload started then lands in the previous folder (a UI gap, see docs/TESTING.md).
  await expect(page.getByRole('heading', { name: folder, level: 1 })).toBeVisible()

  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath('v1'), 'plan.txt', v1))
  await expect(page.getByRole('status')).toHaveText('Uploaded plan.txt.')
  await page.getByRole('button', { name: 'Details of plan.txt' }).click()

  // Saves by the same person within 10 minutes merge into one version (design 5.3); a label
  // keeps the first version on its own.
  await page.getByRole('button', { name: 'Version history' }).click()
  const panel = page.getByRole('region', { name: 'Versions of plan.txt' })
  await expect(panel.getByRole('listitem')).toHaveCount(1)
  await panel.getByRole('button', { name: /^Label version of/ }).click()
  const labelDialog = page.getByRole('dialog', { name: 'Label this version' })
  await labelDialog.getByLabel('Label (empty to remove it)').fill('first')
  await labelDialog.getByRole('button', { name: 'Save label' }).click()
  await expect(panel.getByRole('status')).toHaveText('Label saved.')
  await expect(panel.getByText('first', { exact: true })).toBeVisible()
  await panel.getByRole('button', { name: 'Back to details' }).click()

  // A new version through "Upload new version" (target new_version with the head as base).
  await page.getByLabel('New version of plan.txt').setInputFiles(testFile(info.outputPath('v2'), 'plan.txt', v2))
  await expect(page.getByRole('status')).toHaveText('Uploaded plan.txt.')
  await page.getByRole('button', { name: 'Version history' }).click()
  await expect(panel.getByRole('listitem')).toHaveCount(2)
  const current = panel.getByRole('listitem').filter({ hasText: 'Current' })
  await expect(current).toHaveCount(1)
  await expect(current).not.toContainText('first')
  const first = panel.getByRole('listitem').filter({ hasText: 'first' })

  // The diff of the first version against the current one: the text plugin, line changes.
  await first.getByRole('button', { name: /^Compare version of .* with current$/ }).click()
  const changes = first.getByRole('group', { name: 'Changes from this version to the current one' })
  await expect(changes).toBeVisible()
  await expect(changes).toContainText('compared as text')
  await expect(changes.locator('ins', { hasText: 'blueberries' }).first()).toBeVisible()
  await expect(changes.locator('del', { hasText: 'bananas' }).first()).toBeVisible()
  await expect(changes.locator('ins', { hasText: 'dates' }).first()).toBeVisible()

  // Restore the first version: a new current version with its content; nothing is lost.
  await first.getByRole('button', { name: /^Restore version of/ }).click()
  await page.getByRole('dialog', { name: 'Restore this version?' }).getByRole('button', { name: 'Restore' }).click()
  await expect(panel.getByRole('status')).toHaveText('Version restored.')
  await expect(panel.getByRole('listitem')).toHaveCount(3)
  const restored = panel.getByRole('listitem').filter({ hasText: 'Current' })
  await expect(restored).toContainText('restored from an older version')

  const [dl] = await Promise.all([page.waitForEvent('download'), restored.getByRole('button', { name: /^Download version of/ }).click()])
  const file = info.outputPath('restored.txt')
  await dl.saveAs(file)
  expect(fs.readFileSync(file, 'utf8')).toBe(v1)
})
