// The agents page as Ann: the Connect an agent page, an agent with a key, the agent works through
// the API with that key, its activity with names, and undo.
import { expect, test } from '@playwright/test'
import { env, readRun, storageStatePath } from './support/env.ts'
import { agentToken, api, toast, uniqueName } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

test('register an agent, create a key, see its activity and undo it', async ({ page }) => {
  const { run, users } = readRun()
  const name = uniqueName('Gardener', run)
  await page.goto('/')
  await page.waitForURL(/\/t\/[0-9a-f-]{36}\/files/)
  const tenant = /\/t\/([0-9a-f-]{36})/.exec(page.url())![1]

  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Agents' }).click()
  await expect(page.getByRole('heading', { name: 'Agents', level: 1 })).toBeVisible()
  // Connect an agent: the MCP address of this server and the steps (journeys J5 step 1).
  await page.getByRole('button', { name: 'Connect an agent' }).first().click()
  await expect(page.getByRole('heading', { name: 'Connect an agent', level: 1 })).toBeVisible()
  await expect(page.getByLabel('Server address', { exact: true })).toHaveValue(`${new URL(env.webUrl).origin}/mcp`)
  await expect(page.getByLabel('Command for Claude Code', { exact: true })).toHaveValue(/^claude mcp add --transport http ruum http/)
  await page.getByRole('button', { name: 'Create an agent with a key' }).click()
  const dialog = page.getByRole('dialog', { name: 'Create an agent with a key' })
  await expect(dialog.getByLabel('Name', { exact: true })).toBeFocused()
  await dialog.getByLabel('Name', { exact: true }).fill(name)
  await dialog.getByLabel('Description (optional)').fill('Waters the e2e folders')
  await dialog.getByRole('button', { name: 'Create agent' }).click()

  await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible()
  await expect(page.getByText('Agent created. Now create a key for it below, so it can get to work.')).toBeVisible()
  const activity = page.getByRole('region', { name: 'Activity' })
  await expect(activity).toContainText(`${name} hasn't changed anything yet`)

  // A key, shown once.
  const keys = page.getByRole('region', { name: 'Keys' })
  await keys.getByRole('button', { name: 'New key' }).click()
  const keyDialog = page.getByRole('dialog', { name: `New key for ${name}` })
  await keyDialog.getByLabel('Key name (optional)').fill('e2e')
  await keyDialog.getByRole('button', { name: 'Create key' }).click()
  const secret = await keyDialog.getByLabel('Key', { exact: true }).inputValue()
  expect(secret).toMatch(/^ruum_agent_/)
  await expect(keyDialog).toContainText('Ruum shows it only once.')
  await keyDialog.getByRole('button', { name: 'Done' }).click()
  await expect(keys.getByRole('listitem').filter({ hasText: 'e2e' })).toContainText('never used')

  // The agent works through the API with its key, outside the browser.
  const token = await agentToken(secret)
  const headers = { Authorization: `Bearer ${token}`, 'Ruum-Tenant': tenant, 'Content-Type': 'application/json' }
  const rootRes = await fetch(`${env.serverUrl}/v1/nodes/root`, { headers })
  expect(rootRes.status).toBe(200)
  const root = (await rootRes.json()) as { id: string }
  const folder = uniqueName('Watered', run)
  const created = await fetch(`${env.serverUrl}/v1/nodes`, {
    method: 'POST',
    headers: { ...headers, 'Idempotency-Key': crypto.randomUUID() },
    body: JSON.stringify({ parent_id: root.id, name: folder, kind: 'folder' }),
  })
  expect(created.status).toBe(201)
  // A second folder, which Ann renames afterwards: the undo leaves it and names her (C-11).
  const kept = uniqueName('Kept', run)
  const second = await fetch(`${env.serverUrl}/v1/nodes`, {
    method: 'POST',
    headers: { ...headers, 'Idempotency-Key': crypto.randomUUID() },
    body: JSON.stringify({ parent_id: root.id, name: kept, kind: 'folder' }),
  })
  expect(second.status).toBe(201)
  const keptId = ((await second.json()) as { node: { id: string } }).node.id
  const renamed = `${kept} (Ann's)`
  expect((await api(page, 'POST', `/v1/nodes/${keptId}/rename`, { tenant, body: { name: renamed } })).status).toBe(200)

  // Ann sees the change in the agent's activity, with the item's name and a link.
  await page.reload()
  await expect(activity).toContainText(`Created ${folder}`)
  await expect(activity.getByRole('link', { name: folder })).toBeVisible()
  await expect(keys.getByRole('listitem').filter({ hasText: 'e2e' })).not.toContainText('never used')
  await page.getByRole('main').getByRole('link', { name: 'Agents', exact: true }).click()
  const agentRow = page.getByRole('row').filter({ has: page.getByRole('link', { name }) })
  await expect(agentRow).toContainText('Active, last used')
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Files' }).click()
  const folderRow = page.getByRole('row').filter({ has: page.getByRole('link', { name: folder, exact: true }) })
  // The Changed column names the agent and the person it acts for, with the agent mark; never an ID.
  await expect(folderRow).toContainText(`${name} (agent of ${users.ann.displayName})`)
  await expect(folderRow).toContainText('(an agent)')

  // Undo what the agent did: a dry run first, then the undo; the folder goes away.
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Agents' }).click()
  await page.getByRole('link', { name }).click()
  await activity.getByRole('button', { name: 'Undo all...' }).click()
  const undo = page.getByRole('dialog', { name: `Undo 1 change by ${name}?` })
  await expect(undo).toContainText(`Created ${folder}`)
  // The renamed folder stays as it is, and the dialog says who changed it later (C-11).
  await expect(undo.getByRole('listitem').filter({ hasText: 'changed it again later' })).toContainText(
    new RegExp(`${kept}.*: ${users.ann.displayName} changed it again later`),
  )
  await undo.getByRole('button', { name: 'Undo 1 change' }).click()
  await expect(undo).toHaveCount(0)
  await expect(toast(page, 'Undid 1 change.')).toBeVisible()
  // The undone row stays, marked.
  await expect(activity.getByText('Undone')).toBeVisible()

  // Pause asks first and says what it does (journeys J5 step 9).
  await page.getByRole('button', { name: 'Pause', exact: true }).click()
  const pause = page.getByRole('dialog', { name: `Pause ${name}?` })
  await expect(pause).toContainText("It can't read or change anything until you resume it.")
  await pause.getByRole('button', { name: 'Pause', exact: true }).click()
  await expect(toast(page, `Paused ${name}.`)).toBeVisible()
  await expect(page.getByRole('button', { name: 'Resume' })).toBeVisible()
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Files' }).click()
  await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  await expect(page.getByRole('link', { name: folder, exact: true })).toHaveCount(0)
})
