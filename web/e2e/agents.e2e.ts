// The agents page as Ann: register an agent, create an API key, let the agent work through the
// API with that key, see its activity, and undo it.
import { expect, test } from '@playwright/test'
import { env, readRun, storageStatePath } from './support/env.ts'
import { agentToken, uniqueName } from './support/app.ts'

test.use({ storageState: storageStatePath('ann') })

test('register an agent, create a key, see its activity and undo it', async ({ page }) => {
  const { run, users } = readRun()
  const name = uniqueName('Gardener', run)
  await page.goto('/')
  await page.waitForURL(/\/t\/[0-9a-f-]{36}\/files/)
  const tenant = /\/t\/([0-9a-f-]{36})/.exec(page.url())![1]

  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Agents' }).click()
  await expect(page.getByRole('heading', { name: 'Agents', level: 1 })).toBeVisible()
  await page.getByRole('button', { name: 'Register agent' }).click()
  const dialog = page.getByRole('dialog', { name: 'Register an agent' })
  await dialog.getByLabel('Name', { exact: true }).fill(name)
  await dialog.getByLabel('Description (optional)').fill('Waters the e2e folders')
  await dialog.getByRole('button', { name: 'Register' }).click()

  await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible()
  const agentId = /\/agents\/([0-9a-f-]{36})/.exec(page.url())![1]
  await expect(page.getByRole('status').filter({ hasText: 'Agent registered. Create a key for it below.' })).toBeVisible()
  const activity = page.getByRole('region', { name: 'Activity' })
  await expect(activity).toContainText('No changes by this agent.')

  // A key, shown once.
  const keys = page.getByRole('region', { name: 'API keys' })
  await keys.getByRole('button', { name: 'New key' }).click()
  const keyDialog = page.getByRole('dialog', { name: 'New API key' })
  await keyDialog.getByLabel('Name (optional)').fill('e2e')
  await keyDialog.getByRole('button', { name: 'Create key' }).click()
  const secret = await keyDialog.getByLabel('API key').inputValue()
  expect(secret).toMatch(/^ruum_agent_/)
  await expect(keyDialog).toContainText('Ruum shows it only once.')
  await keyDialog.getByRole('button', { name: 'Done' }).click()
  await expect(keys.getByRole('listitem').filter({ hasText: 'e2e' })).toContainText('last used never')

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

  // Ann sees the change in the agent's activity, and in the file list with both names.
  await page.reload()
  await expect(activity.getByText(`Created: ${folder}`)).toBeVisible()
  await expect(keys.getByRole('listitem').filter({ hasText: 'e2e' })).not.toContainText('last used never')
  await page.getByRole('link', { name: 'Back to agents' }).click()
  const agentRow = page.getByRole('row').filter({ has: page.getByRole('link', { name }) })
  await expect(agentRow).toContainText('Active')
  await expect(agentRow).not.toContainText('Never')
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Files' }).click()
  const folderRow = page.getByRole('row').filter({ has: page.getByRole('link', { name: folder, exact: true }) })
  // The change names the agent and the user it acts for. (The node's actor carries no display name
  // for agents, so the UI shows the first 8 characters of the agent ID; a small gap.)
  await expect(folderRow.getByRole('cell').nth(1)).toHaveAttribute('title', `${agentId.slice(0, 8)} (agent for ${users.ann.displayName})`)

  // Undo what the agent did: a dry run first, then the undo; the folder goes away.
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Agents' }).click()
  await page.getByRole('link', { name }).click()
  await activity.getByRole('button', { name: 'Undo all...' }).click()
  const undo = page.getByRole('dialog', { name: 'Undo changes?' })
  await expect(undo).toContainText('1 change can be undone.')
  await undo.getByRole('button', { name: 'Undo 1 change' }).click()
  await expect(undo).toHaveCount(0)
  await page.getByRole('navigation', { name: 'Main' }).getByRole('link', { name: 'Files' }).click()
  await expect(page.getByRole('heading', { name: 'Files', level: 1 })).toBeVisible()
  await expect(page.getByRole('link', { name: folder, exact: true })).toHaveCount(0)
})
