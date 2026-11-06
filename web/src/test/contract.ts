// Checks each request of the tests against contracts/openapi.yaml: the JSON body against the
// request body schema, the rules that the contract states only in words (for example "give since
// or seqs"), and whether the `Ruum-Tenant` header is allowed. The mock API calls it through
// `setRequestCheck` (src/mocks/handlers.ts), so a request that the real server refuses fails the
// test too.

import Ajv2020, { type ValidateFunction } from 'ajv/dist/2020'
import { parse } from 'yaml'
import spec from '../../../contracts/openapi.yaml?raw'
import type { RequestCheck } from '../mocks/handlers'

type Json = Record<string, unknown>

const doc = parse(spec) as Json

const ajv = new Ajv2020({ strict: false, allErrors: true })
ajv.addFormat('uuid', /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i)
ajv.addFormat('date-time', (v: string) => /^\d{4}-\d{2}-\d{2}T/.test(v) && !Number.isNaN(Date.parse(v)))
ajv.addFormat('email', /^[^@\s]+@[^@\s]+$/)
ajv.addFormat('uri', (v: string) => URL.canParse(v))
ajv.addFormat('int64', { type: 'number', validate: (v: number) => Number.isInteger(v) })
ajv.addSchema(doc, 'openapi')

/** Rules that the contract states in a description and that a JSON schema does not express. */
const extraRules: Record<string, (body: Json) => string | null> = {
  UndoRequest: (b) => (b.since !== undefined || b.seqs !== undefined ? null : 'UndoRequest needs since or seqs.'),
}

function resolve(node: Json): Json {
  let cur = node
  while (typeof cur.$ref === 'string') {
    const parts = cur.$ref.replace(/^#\//, '').split('/')
    cur = parts.reduce<Json>((o, p) => o[p.replace(/~1/g, '/').replace(/~0/g, '~')] as Json, doc)
  }
  return cur
}

const pointer = (...parts: string[]) => parts.map((p) => p.replace(/~/g, '~0').replace(/\//g, '~1')).join('/')

type Compiled = { takesTenant: boolean; required: boolean; schemaRef?: string; validate?: ValidateFunction }
const cache = new Map<string, Compiled | null>()

function compile(method: string, path: string): Compiled | null {
  const key = `${method} ${path}`
  if (cache.has(key)) return cache.get(key)!
  const paths = doc.paths as Record<string, Json>
  const item = paths[path]
  const op = item?.[method] as Json | undefined
  if (!item || !op) {
    cache.set(key, null)
    return null
  }
  const params = [...((item.parameters as Json[]) ?? []), ...((op.parameters as Json[]) ?? [])]
  const takesTenant = params.some((p) => p.$ref === '#/components/parameters/TenantHeader')
  let compiled: Compiled = { takesTenant, required: false }
  if (op.requestBody) {
    const rb = resolve(op.requestBody as Json)
    const content = rb.content as Record<string, Json> | undefined
    const schema = content?.['application/json']?.schema as Json | undefined
    if (schema) {
      const base = (op.requestBody as Json).$ref
        ? `openapi${String((op.requestBody as Json).$ref)}`
        : `openapi#/${pointer('paths', path, method, 'requestBody')}`
      compiled = {
        takesTenant,
        required: Boolean(rb.required),
        schemaRef: typeof schema.$ref === 'string' ? schema.$ref.split('/').at(-1) : undefined,
        validate: ajv.compile({ $ref: `${base}/content/application~1json/schema` }),
      }
    }
  }
  cache.set(key, compiled)
  return compiled
}

/** The check for `setRequestCheck`. Returns the problems of one request. */
export const contractCheck: RequestCheck = ({ method, path, headers, body, jsonBody }) => {
  const c = compile(method, path)
  if (!c) return [`${method.toUpperCase()} ${path} is not in the contract.`]
  const problems: string[] = []
  const tenant = headers.get('Ruum-Tenant')
  if (tenant && !c.takesTenant) problems.push('sends Ruum-Tenant, but the operation takes no tenant header.')
  if (!tenant && c.takesTenant) problems.push('needs the Ruum-Tenant header for a user session.')
  if (c.validate && jsonBody) {
    if (body === undefined) {
      if (c.required) problems.push('the request body is missing.')
    } else if (!c.validate(body)) {
      for (const e of c.validate.errors ?? []) problems.push(`body${e.instancePath} ${e.message ?? 'is not valid'}`)
    } else if (c.schemaRef && extraRules[c.schemaRef]) {
      const p = extraRules[c.schemaRef](body as Json)
      if (p) problems.push(p)
    }
  }
  return problems
}
