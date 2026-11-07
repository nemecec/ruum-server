// Error copy for people (docs/ux/copy.md section 4, design 12.4). The API's `message` and `hint`
// are written for agents and developers, in English; the UI shows them only in the "Details"
// disclosure of an error note. The copy is in the catalogue, one message per error code.

import { t, type MessageKey } from '../i18n'
import { RuumApiError, type ApiError } from './client'

/** What the UI knows about the place of the error, to fill in the copy. */
export type ErrorContext = {
  /** The name of the item the person worked on, for `name_taken` and `version_mismatch`. */
  name?: string
  /** The space, for `permission_denied` and `quota_exceeded`. */
  space?: string
}

type Code = ApiError['code']

/** A name in the message of a `name_taken` answer: `... named "Plan" ...`. */
function nameInMessage(message: string): string | undefined {
  return /named "([^"]+)"/.exec(message)?.[1] ?? /"([^"]+)"/.exec(message)?.[1]
}

/** The limit of a `plan_limit_reached` answer: `... its limit of 1 members ...` gives "members". */
function limitInMessage(message: string): string | undefined {
  return /limit of \d+ ([a-z ]+?)(?:[.,;]|\s+(?:invite|remove|move|or)\b|$)/i.exec(message)?.[1]?.trim()
}

/** The limits of a plan that the copy names. Another limit gets the copy without a name. */
const knownLimits = new Set(['members', 'agents'])

/** The copy of an error code that needs only the context. */
const simple = (key: MessageKey) => (_e: { message: string }, ctx: ErrorContext) =>
  t(key, { name: ctx.name ?? '', space: ctx.space ?? '', hasName: ctx.name ? 'yes' : 'no', hasSpace: ctx.space ? 'yes' : 'no' })

/**
 * The copy of each error code, from the catalogue (`error.<code>`). Codes marked "agents only"
 * never reach people in the web UI; the entry is a safe fallback all the same. A code that the UI
 * does not know (a newer server) shows the server's message.
 */
export const errorCopy: Record<Code, (e: { message: string }, ctx: ErrorContext) => string> = {
  bad_request: simple('error.bad_request'),
  validation_failed: simple('error.validation_failed'),
  unauthenticated: simple('error.unauthenticated'),
  token_expired: simple('error.unauthenticated'),
  permission_denied: simple('error.permission_denied'),
  scope_exceeded: simple('error.scope_exceeded'),
  user_required: simple('error.user_required'),
  not_found: simple('error.not_found'),
  conflict: simple('error.conflict'),
  name_taken: (e, ctx) => {
    const name = ctx.name ?? nameInMessage(e.message)
    return name ? t('error.name_taken.named', { name }) : t('error.name_taken')
  },
  version_mismatch: simple('error.version_mismatch'),
  precondition_failed: simple('error.version_mismatch'),
  idempotency_key_reused: simple('error.idempotency_key_reused'),
  payload_too_large: simple('error.payload_too_large'),
  range_not_satisfiable: simple('error.try_again'),
  cursor_expired: simple('error.try_again'),
  quota_exceeded: simple('error.quota_exceeded'),
  plan_limit_reached: (e) => {
    const what = limitInMessage(e.message)
    return t('error.plan_limit_reached', { limit: what && knownLimits.has(what) ? what : 'other' })
  },
  rate_limited: simple('error.rate_limited'),
  agent_disabled: simple('error.agent_disabled'),
  chunk_missing: simple('error.upload_incomplete'),
  hash_mismatch: simple('error.upload_incomplete'),
  // Step 2 (databases). The database pages (card U-2) may refine this copy.
  database_not_ready: simple('error.database_not_ready'),
  query_failed: simple('error.query_failed'),
  read_only_query: simple('error.read_only_query'),
  statement_timeout: simple('error.statement_timeout'),
  write_grant_required: simple('error.write_grant_required'),
  migration_failed: simple('error.migration_failed'),
  database_too_large: simple('error.database_too_large'),
  statement_not_allowed: simple('error.statement_not_allowed'),
  class_read_required: simple('error.class_read_required'),
  not_implemented: simple('error.not_implemented'),
  internal: simple('error.internal'),
  unavailable: simple('error.internal'),
}

/**
 * The text of an error for people: the localised copy of its code. Never the API's message or
 * hint, except for a code the UI does not know: then the server's message is the best there is.
 */
export function errorText(e: unknown, ctx: ErrorContext = {}): string {
  if (e instanceof RuumApiError) {
    const copy = (errorCopy as Partial<Record<string, (typeof errorCopy)[Code]>>)[e.code]
    if (copy) return copy(e, ctx)
    return e.message ? sentence(e.message) : t('error.try_again')
  }
  if (e instanceof DOMException && e.name === 'AbortError') return t('error.cancelled')
  if (e instanceof TypeError) return t('error.network')
  if (e instanceof Error && e.message) return e.message
  return t('error.network')
}

/** A server message as a sentence: a capital first letter and a full stop. */
function sentence(message: string): string {
  const m = message.trim()
  const s = m.charAt(0).toUpperCase() + m.slice(1)
  return /[.!?]$/.test(s) ? s : `${s}.`
}

/** The facts for the "Details" disclosure: code, the API's message and hint. */
export function errorDetails(e: unknown): string | null {
  if (e instanceof RuumApiError) return [t('error.details.code', { code: e.code, status: e.status }), e.message, e.hint].filter(Boolean).join('\n')
  if (e instanceof Error) return e.message
  return null
}
