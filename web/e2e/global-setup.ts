// Runs once before the tests: checks that the server and the web UI answer, registers the web UI's
// callback in Zitadel, and makes new Zitadel users for this run: Ann and Ben sign in during the
// setup project; Cara signs in for the first time in the sharing spec, after Ann shared with her.
import crypto from 'node:crypto'
import fs from 'node:fs'
import { env, statePath, type RunState, type TestUser } from './support/env.ts'
import { createUser, ensureRedirectUri } from './support/zitadel.ts'

async function up(url: string) {
  const res = await fetch(url).catch((e: Error) => {
    throw new Error(`${url} does not answer (${e.message}); start the server and the web UI first`)
  })
  if (!res.ok) throw new Error(`${url} answered ${res.status}`)
}

export default async function globalSetup() {
  await up(`${env.serverUrl}/healthz`)
  await up(`${env.webUrl}/healthz`)
  await ensureRedirectUri(env.webUrl)

  const run = crypto.randomBytes(3).toString('hex')
  const user = async (key: TestUser['key'], given: string): Promise<TestUser> => {
    const email = `${key}-${run}@example.test`
    // A random test password for this run's throwaway user (upper, lower, digit and symbol).
    const password = `Aa1!${crypto.randomBytes(12).toString('base64url')}`
    await createUser({ email, password, givenName: given, familyName: run })
    return { key, loginName: email, email, password, displayName: `${given} ${run}` }
  }
  const state: RunState = { run, users: { ann: await user('ann', 'Ann'), ben: await user('ben', 'Ben'), cara: await user('cara', 'Cara') } }
  fs.mkdirSync(env.workDir, { recursive: true })
  fs.writeFileSync(statePath(), JSON.stringify(state, null, 2), { mode: 0o600 })
  console.log(`e2e run ${run}: Zitadel users ${Object.values(state.users).map((u) => u.email).join(', ')}`)
}
