// Browser end-to-end tests against the real server, the laptop stack and the web UI (Vite).
// Run them with scripts/e2e_browser.sh, which starts the server on 8492 and Vite on 5182.
// Chromium only. The tests are in e2e/ and end in `.e2e.ts`, so Vitest does not pick them up.
import os from 'node:os'
import path from 'node:path'
import { defineConfig, devices } from '@playwright/test'

const work = process.env.RUUM_E2E_WORK ?? path.join(os.tmpdir(), 'ruum-e2e-browser')

export default defineConfig({
  testDir: './e2e',
  testMatch: /.*\.e2e\.ts/,
  outputDir: path.join(work, 'results'),
  globalSetup: './e2e/global-setup.ts',
  // One server, one tenant per user: the specs run one after the other.
  workers: 1,
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  reporter: [['list'], ['html', { outputFolder: path.join(work, 'report'), open: 'never' }]],
  use: {
    baseURL: process.env.RUUM_E2E_WEB_URL ?? 'http://localhost:5182',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    actionTimeout: 15_000,
  },
  projects: [
    { name: 'setup', testMatch: /auth\.setup\.e2e\.ts/, use: { ...devices['Desktop Chrome'] } },
    {
      name: 'chromium',
      testIgnore: /auth\.setup\.e2e\.ts/,
      dependencies: ['setup'],
      use: { ...devices['Desktop Chrome'] },
    },
  ],
})
