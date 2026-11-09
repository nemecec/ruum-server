import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'
import spec from '../../../contracts/openapi.yaml?raw'
import { RuumApiError } from '../api/client'
import { errorCopy, errorText } from '../api/errors'

const codes = (parse(spec) as { components: { schemas: { ErrorCode: { enum: string[] } } } }).components.schemas.ErrorCode.enum

describe('error copy (copy.md section 4)', () => {
  it('has copy for every error code of the contract', () => {
    expect(Object.keys(errorCopy).sort()).toEqual([...codes].sort())
  })

  it.each(codes)('never shows the API message or hint for %s', (code) => {
    const e = new RuumApiError(400, { code: code as never, message: 'the node has an internal problem', hint: 'send on_conflict: rename' })
    const text = errorText(e)
    expect(text).not.toMatch(/node|tenant|on_conflict|hint/i)
    expect(text[0]).toBe(text[0].toUpperCase())
    expect(text.endsWith('.')).toBe(true)
  })

  it('names the item of a name conflict, from the context or the message', () => {
    const e = new RuumApiError(409, { code: 'name_taken', message: 'the folder already has a node named "Pärna 12"', hint: 'pick another name, or send on_conflict: rename' })
    expect(errorText(e)).toBe("There's already an item called Pärna 12 here. Pick another name.")
    expect(errorText(e, { name: 'Plan' })).toBe("There's already an item called Plan here. Pick another name.")
  })

  it('names the limit of a plan', () => {
    const e = new RuumApiError(403, { code: 'plan_limit_reached', message: 'the tenant has reached its limit of 1 members', hint: 'invite the person as a guest, or move to a team plan' })
    expect(errorText(e)).toBe("You've reached the member limit of your plan. Remove a member, or an admin can change the plan.")
  })

  it('falls back to the server message for a code the UI does not know', () => {
    const e = new RuumApiError(409, { code: 'branch_locked' as never, message: 'the branch is locked by a migration', hint: 'wait' })
    expect(errorText(e)).toBe('The branch is locked by a migration.')
  })

  it('shows the copy in the language of the UI', async () => {
    const { currentIntl, setCurrentIntl, makeIntl } = await import('../i18n/intl')
    const { loadMessages } = await import('../i18n/catalogue')
    const before = currentIntl()
    setCurrentIntl(makeIntl('et', await loadMessages('et')))
    try {
      const e = new RuumApiError(409, { code: 'name_taken', message: 'the folder already has a node named "Plaan"', hint: '' })
      expect(errorText(e)).toBe('Siin on juba üksus nimega Plaan. Vali teine nimi.')
      expect(errorText(new RuumApiError(403, { code: 'permission_denied', message: 'no', hint: '' }), { space: 'Pärn' })).toBe(
        'Sul pole selleks ligipääsu. Küsi ruumi Pärn omanikult või administraatorilt.',
      )
    } finally {
      setCurrentIntl(before)
    }
  })
})
