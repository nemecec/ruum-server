// Browser end-to-end tests against the real server, the laptop stack and the web UI (Vite).
// Run them with scripts/e2e_browser.sh, which starts the server on 8492 and Vite on 5182.
// Chromium only. The tests are in e2e/ and end in `.e2e.ts`, so Vitest does not pick them up.
//
// The `visual` project (e2e/visual.e2e.ts) needs no server: it runs the UI with the mock API
// (`vite --mode mock`, started here on RUUM_E2E_MOCK_PORT, default 5187), with a fixed clock, and
// compares screenshots of the main screens with the snapshots in e2e/visual.e2e.ts-snapshots/,
// and runs axe on each. Alone, without the stack:
//   RUUM_E2E_MOCK_ONLY=1 npx playwright test --project visual [--update-snapshots]
import os from 'node:os'
import path from 'node:path'
import { defineConfig, devices } from '@playwright/test'

const work = process.env.RUUM_E2E_WORK ?? path.join(os.tmpdir(), 'ruum-e2e-browser')
const mockPort = Number(process.env.RUUM_E2E_MOCK_PORT ?? 5187)
const mockOnly = Boolean(process.env.RUUM_E2E_MOCK_ONLY)

export default defineConfig({
  testDir: './e2e',
  testMatch: /.*\.e2e\.ts/,
  outputDir: path.join(work, 'results'),
  globalSetup: mockOnly ? undefined : './e2e/global-setup.ts',
  // Snapshots carry the platform in their name (fonts differ); they are made on macOS.
  snapshotPathTemplate: '{testDir}/{testFilePath}-snapshots/{arg}-{projectName}-{platform}{ext}',
  // One server, one tenant per user: the specs run one after the other.
  workers: 1,
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: 0,
  timeout: 90_000,
  expect: { timeout: 15_000, toHaveScreenshot: { maxDiffPixelRatio: 0.01, threshold: 0.2, animations: 'disabled' } },
  reporter: [['list'], ['html', { outputFolder: path.join(work, 'report'), open: 'never' }]],
  use: {
    baseURL: process.env.RUUM_E2E_WEB_URL ?? 'http://localhost:5182',
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    actionTimeout: 15_000,
  },
  // The mock UI of the visual project.
  webServer: {
    command: `node node_modules/vite/bin/vite.js --mode mock --port ${mockPort} --strictPort`,
    url: `http://127.0.0.1:${mockPort}/`,
    reuseExistingServer: !process.env.CI,
    stdout: 'ignore',
    stderr: 'pipe',
    env: { RUUM_WEB_PORT: String(mockPort) },
  },
  projects: [
    ...(mockOnly
      ? []
      : [
          { name: 'setup', testMatch: /auth\.setup\.e2e\.ts/, use: { ...devices['Desktop Chrome'] } },
          {
            name: 'chromium',
            testIgnore: [/auth\.setup\.e2e\.ts/, /visual\.e2e\.ts/],
            dependencies: ['setup'],
            use: { ...devices['Desktop Chrome'] },
          },
        ]),
    {
      name: 'visual',
      testMatch: /visual\.e2e\.ts/,
      use: {
        ...devices['Desktop Chrome'],
        baseURL: `http://localhost:${mockPort}`,
        locale: 'en-GB',
        timezoneId: 'Europe/Tallinn',
      },
    },
  ],
})
