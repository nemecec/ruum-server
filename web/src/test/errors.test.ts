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
    expect(errorText(e)).toBe("You've reached the limit of your plan for members. Remove one you don't use, or change the plan.")
  })
})
