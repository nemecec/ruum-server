// The file browser as Ann: folders, a chunked upload, download and compare, rename, move, trash
// and restore.
import crypto from 'node:crypto'
import fs from 'node:fs'
import { expect, test, type Page } from '@playwright/test'
import { readRun, storageStatePath } from './support/env.ts'
import { sha256, testFile, toast, uniqueName } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

async function openDetails(page: Page, name: string) {
  await page.getByRole('button', { name: `Details of ${name}`, exact: true }).click()
  await expect(page.getByRole('heading', { name, level: 2 })).toBeVisible()
}

async function download(page: Page, dir: string): Promise<Buffer> {
  const [dl] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Download', exact: true }).click()])
  const file = `${dir}/${crypto.randomUUID()}-${dl.suggestedFilename()}`
  await dl.saveAs(file)
  return fs.readFileSync(file)
}

test('folders, chunked upload, download, rename, move, trash and restore', async ({ page }, info) => {
  const { run } = readRun()
  const top = uniqueName('Box', run)
  await page.goto('/')
  await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()

  // New folder at the top level, then a subfolder inside it.
  await page.getByRole('button', { name: 'New folder' }).first().click()
  const dialog = page.getByRole('dialog', { name: 'New folder' })
  await dialog.getByLabel('Folder name').fill(top)
  await dialog.getByRole('button', { name: 'Create' }).click()
  await expect(toast(page, `Folder ${top} created.`)).toBeVisible()
  await page.getByRole('link', { name: top, exact: true }).click()
  await expect(page.getByRole('heading', { name: top, level: 1 })).toBeVisible()
  await expect(page.getByRole('navigation', { name: 'Breadcrumbs' })).toContainText(top)
  await expect(page.getByText('This folder is empty', { exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill('Archive')
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await expect(page.getByRole('link', { name: 'Archive', exact: true })).toBeVisible()

  // A 3 MB upload: the browser cuts it with FastCDC and sends the chunks one by one.
  const data = crypto.randomBytes(3 * 1024 * 1024)
  const file = testFile(info.outputPath(), 'big.bin', data)
  const chunkPuts: string[] = []
  page.on('request', (r) => {
    if (r.method() === 'PUT' && /\/v1\/uploads\/[^/]+\/chunks\//.test(r.url())) chunkPuts.push(r.url())
  })
  await page.getByLabel('Upload files').setInputFiles(file)
  await expect(toast(page, 'Uploaded big.bin.')).toBeVisible({ timeout: 60_000 })
  // The upload panel at the bottom right says so too.
  await expect(page.getByRole('region', { name: 'Uploads' })).toContainText('Done')
  expect(chunkPuts.length, 'chunks sent').toBeGreaterThan(1)
  const row = page.getByRole('row', { name: /big\.bin/ })
  await expect(row).toContainText('3.0 MB')

  // Download through the content domain and compare the bytes.
  await openDetails(page, 'big.bin')
  const back = await download(page, info.outputPath())
  expect(back.length).toBe(data.length)
  expect(sha256(back)).toBe(sha256(data))

  // Rename.
  await page.getByRole('button', { name: 'Rename' }).click()
  const rename = page.getByRole('dialog', { name: 'Rename big.bin' })
  // The dialog puts the focus in the field and selects the name without its extension, as file
  // managers do: typing keeps ".bin".
  const nameInput = rename.getByLabel('Name')
  await expect(nameInput).toBeFocused()
  await page.keyboard.type('renamed')
  await expect(nameInput).toHaveValue('renamed.bin')
  await rename.getByRole('button', { name: 'Rename' }).click()
  await expect(page.getByRole('button', { name: 'renamed.bin', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'big.bin', exact: true })).toHaveCount(0)

  // Move into Archive with the folder picker.
  await openDetails(page, 'renamed.bin')
  await page.getByRole('button', { name: 'Move', exact: true }).click()
  const move = page.getByRole('dialog', { name: 'Move renamed.bin' })
  await move.getByRole('button', { name: 'Archive', exact: true }).click()
  await expect(move.getByRole('navigation', { name: 'Folder path' })).toContainText('Archive')
  await move.getByRole('button', { name: 'Move here' }).click()
  await expect(toast(page, 'Moved renamed.bin to Archive.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'renamed.bin', exact: true })).toHaveCount(0)
  await page.getByRole('link', { name: 'Archive', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Archive', level: 1 })).toBeVisible()
  await expect(page.getByRole('button', { name: 'renamed.bin', exact: true })).toBeVisible()

  // Trash (it can be undone, so no confirmation), then restore from the trash page.
  await openDetails(page, 'renamed.bin')
  await page.getByRole('button', { name: 'Move to trash' }).click()
  await expect(toast(page, 'Moved renamed.bin to trash.')).toBeVisible()
  await expect(page.getByText('This folder is empty', { exact: true })).toBeVisible()
  const archiveUrl = page.url()

  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Trash' }).click()
  await expect(page.getByRole('heading', { name: 'Trash', level: 1 })).toBeVisible()
  await expect(page.getByRole('rowheader', { name: 'renamed.bin' })).toBeVisible()
  await page.getByRole('button', { name: 'Restore renamed.bin' }).first().click()
  await expect(toast(page, /^Restored renamed\.bin to Archive/)).toBeVisible()

  await page.goto(archiveUrl)
  await expect(page.getByRole('button', { name: 'renamed.bin', exact: true })).toBeVisible()
  await openDetails(page, 'renamed.bin')
  expect(sha256(await download(page, info.outputPath()))).toBe(sha256(data))
})

test('a second upload of the same bytes sends no chunks (dedup)', async ({ page }, info) => {
  const dedup = uniqueName('Dedup', readRun().run)
  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(dedup)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name: dedup, exact: true }).click()
  await expect(page.getByRole('heading', { name: dedup, level: 1 })).toBeVisible()

  const data = crypto.randomBytes(512 * 1024)
  const first = testFile(info.outputPath(), 'one.bin', data)
  const second = testFile(info.outputPath(), 'two.bin', data)
  let puts = 0
  page.on('request', (r) => {
    if (r.method() === 'PUT' && /\/v1\/uploads\/[^/]+\/chunks\//.test(r.url())) puts++
  })
  await page.getByLabel('Upload files').setInputFiles(first)
  await expect(toast(page, 'Uploaded one.bin.')).toBeVisible({ timeout: 60_000 })
  expect(puts).toBeGreaterThan(0)
  puts = 0
  await page.getByLabel('Upload files').setInputFiles(second)
  await expect(toast(page, 'Uploaded two.bin.')).toBeVisible({ timeout: 60_000 })
  expect(puts, 'chunks sent for identical bytes').toBe(0)
})

// Every dialog puts the focus on its first field, or on Cancel in a confirmation, never on Close:
// a keyboard user who types and presses Enter makes the folder.
test('the New folder dialog puts the focus in its name field, and Enter creates the folder', async ({ page }) => {
  const name = uniqueName('Keys', readRun().run)
  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().focus()
  await page.keyboard.press('Enter')
  const field = page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name')
  await expect(field).toBeFocused()
  await expect(field).toHaveValue('')
  await page.keyboard.type(name)
  await page.keyboard.press('Enter')
  await expect(page.getByRole('link', { name, exact: true })).toBeVisible()
})

// While a folder loads after a click, the page shows no toolbar of the folder before, so an upload
// started in that moment waits for the new folder.
test('an upload right after opening a folder lands in that folder', async ({ page }, info) => {
  const name = uniqueName('Quick', readRun().run)
  await page.goto('/')
  await page.getByRole('button', { name: 'New folder' }).first().click()
  await page.getByRole('dialog', { name: 'New folder' }).getByLabel('Folder name').fill(name)
  await page.getByRole('dialog', { name: 'New folder' }).getByRole('button', { name: 'Create' }).click()
  await page.getByRole('link', { name, exact: true }).click()
  // A person can only press Upload once the folder is shown. Setting the file before that reached
  // the previous view's input, which was then removed, and the upload was lost (flaky on CI).
  await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible()
  await page.getByLabel('Upload files').setInputFiles(testFile(info.outputPath(), 'quick.txt', 'quick\n'))
  await expect(toast(page, 'Uploaded quick.txt.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'quick.txt', exact: true })).toBeVisible()
})
