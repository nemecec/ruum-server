// Error copy for people (docs/ux/copy.md section 4). The API's `message` and `hint` are written for
// agents and developers; the UI shows them only in the "Details" disclosure of an error note.

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

/**
 * The copy of each error code. Codes marked "agents only" never reach people in the web UI; the
 * entry is a safe fallback all the same.
 */
export const errorCopy: Record<Code, (e: { message: string }, ctx: ErrorContext) => string> = {
  bad_request: () => "Something in this form isn't right. Check the marked fields.",
  validation_failed: () => "Something in this form isn't right. Check the marked fields.",
  unauthenticated: () => "You've been signed out. Sign in again to keep working.",
  token_expired: () => "You've been signed out. Sign in again to keep working.",
  permission_denied: (_e, ctx) =>
    `You don't have access to do this. Ask the owner or an admin of ${ctx.space ?? 'this space'}.`,
  scope_exceeded: () => "This agent doesn't have access to do this.",
  user_required: () => 'Only a person can do this, not an agent or a link.',
  not_found: () => 'This item is not here any more. It may have been moved or deleted.',
  conflict: () => 'This changed while you were working on it. Refresh to see the newest version, then try again.',
  name_taken: (e, ctx) => {
    const name = ctx.name ?? nameInMessage(e.message)
    return name ? `There's already an item called ${name} here. Pick another name.` : "There's already an item with this name here. Pick another name."
  },
  version_mismatch: (_e, ctx) => `Someone else changed ${ctx.name ?? 'this item'} a moment ago. Refresh to see their change.`,
  precondition_failed: (_e, ctx) => `Someone else changed ${ctx.name ?? 'this item'} a moment ago. Refresh to see their change.`,
  idempotency_key_reused: () => 'Ruum is still working on your last request. Wait a moment.',
  payload_too_large: (_e, ctx) => `${ctx.name ?? 'This file'} is larger than the largest file your plan allows.`,
  range_not_satisfiable: () => "Ruum couldn't do this just now. Try again in a moment.",
  cursor_expired: () => "Ruum couldn't do this just now. Try again in a moment.",
  quota_exceeded: (_e, ctx) => `${ctx.space ?? 'This space'} is full. Free some space, or an admin can change the plan.`,
  plan_limit_reached: (e) => {
    const what = limitInMessage(e.message)
    return `You've reached the limit of your plan${what ? ` for ${what}` : ''}. Remove one you don't use, or change the plan.`
  },
  rate_limited: () => 'That was a lot at once. Wait a minute and try again.',
  agent_disabled: () => 'This agent is paused. Resume it on the Agents page.',
  chunk_missing: (_e, ctx) => `${ctx.name ?? 'The file'} didn't upload completely. Try again.`,
  hash_mismatch: (_e, ctx) => `${ctx.name ?? 'The file'} didn't upload completely. Try again.`,
  not_implemented: () => "This isn't available yet.",
  internal: () => "Ruum couldn't do this just now. Try again in a moment. If it keeps happening, contact support with the code below.",
  unavailable: () => "Ruum couldn't do this just now. Try again in a moment. If it keeps happening, contact support with the code below.",
}

const networkCopy = "Ruum couldn't do this just now. Try again in a moment."

/** The text of an error for people. Never the API's message or hint. */
export function errorText(e: unknown, ctx: ErrorContext = {}): string {
  if (e instanceof RuumApiError) {
    const copy = errorCopy[e.code]
    return copy ? copy(e, ctx) : networkCopy
  }
  if (e instanceof DOMException && e.name === 'AbortError') return 'Cancelled.'
  if (e instanceof TypeError) return networkCopy
  if (e instanceof Error && e.message) return e.message
  return networkCopy
}

/** The facts for the "Details" disclosure: code, the API's message and hint. */
export function errorDetails(e: unknown): string | null {
  if (e instanceof RuumApiError) return [`Code: ${e.code} (${e.status})`, e.message, e.hint].filter(Boolean).join('\n')
  if (e instanceof Error) return e.message
  return null
}
