// Settings of the browser end-to-end tests. scripts/e2e_browser.sh sets them; the defaults match it.
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'

function need(name: string): string {
  const v = process.env[name]
  if (!v) throw new Error(`${name} is not set; run the tests through scripts/e2e_browser.sh or eval "$(scripts/dev.sh env)"`)
  return v
}

export const env = {
  /** The web UI (Vite), which forwards /v1 to the server. */
  webUrl: process.env.RUUM_E2E_WEB_URL ?? 'http://localhost:5182',
  /** The server itself (for calls as an agent, outside the browser). */
  serverUrl: process.env.RUUM_E2E_SERVER_URL ?? 'http://127.0.0.1:8492',
  mailpitUrl: process.env.RUUM_E2E_MAILPIT_URL ?? 'http://127.0.0.1:8025',
  /** A folder for the run: the test users and the signed-in browser states. */
  workDir: process.env.RUUM_E2E_WORK ?? path.join(os.tmpdir(), 'ruum-e2e-browser'),
  zitadel: () => ({
    issuer: need('ZITADEL_ISSUER'),
    projectId: need('ZITADEL_PROJECT_ID'),
    webClientId: need('ZITADEL_WEB_CLIENT_ID'),
    pat: fs.readFileSync(need('ZITADEL_SERVICE_PAT_FILE'), 'utf8').trim(),
  }),
}

export interface TestUser {
  key: 'ann' | 'ben' | 'cara'
  loginName: string
  email: string
  password: string
  displayName: string
}

export interface RunState {
  run: string
  users: Record<TestUser['key'], TestUser>
}

export const statePath = () => path.join(env.workDir, 'run.json')
export const storageStatePath = (key: TestUser['key']) => path.join(env.workDir, `${key}.storage.json`)

export function readRun(): RunState {
  return JSON.parse(fs.readFileSync(statePath(), 'utf8')) as RunState
}
