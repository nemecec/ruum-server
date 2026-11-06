import { describe, expect, it } from 'vitest'
import { ids } from '../../mocks/db'
import { contractCheck } from '../contract'

const tenant = new Headers({ 'Ruum-Tenant': ids.personal })

describe('contract check of the mock API', () => {
  it('refuses an undo without since or seqs', () => {
    expect(
      contractCheck({ method: 'post', path: '/v1/agents/{agent_id}/undo', headers: tenant, body: { dry_run: true }, jsonBody: true }),
    ).toEqual(['UndoRequest needs since or seqs.'])
    expect(
      contractCheck({
        method: 'post',
        path: '/v1/agents/{agent_id}/undo',
        headers: tenant,
        body: { dry_run: true, seqs: [1] },
        jsonBody: true,
      }),
    ).toEqual([])
  })

  it('refuses a body that breaks the schema', () => {
    const problems = contractCheck({
      method: 'post',
      path: '/v1/agents/{agent_id}/undo',
      headers: tenant,
      body: { dry_run: 'yes', seqs: [] },
      jsonBody: true,
    })
    expect(problems.length).toBeGreaterThan(0)
  })

  it('refuses the tenant header where the operation takes none, and needs it where it does', () => {
    expect(contractCheck({ method: 'get', path: '/v1/shared-with-me', headers: tenant, body: undefined, jsonBody: true })).toEqual([
      'sends Ruum-Tenant, but the operation takes no tenant header.',
    ])
    expect(contractCheck({ method: 'get', path: '/v1/nodes/root', headers: new Headers(), body: undefined, jsonBody: true })).toEqual([
      'needs the Ruum-Tenant header for a user session.',
    ])
  })
})
