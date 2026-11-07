import { client, unwrap, type Schemas } from '../api/client'

/**
 * The names of spaces that the person knows only from "Shared with me" (C-10, decision 0003-6):
 * every shared item names its space (`SharedItem.tenant_name`), also a space that is not in the
 * person's list of spaces. Kept for the session, so the shell can name such a space.
 */
const names = new Map<string, string>()

/** Keeps the space names of shared items. */
export function rememberSpaceNames(items: readonly Schemas['SharedItem'][]): void {
  for (const i of items) if (i.tenant_name) names.set(i.node.tenant_id, i.tenant_name)
}

/** The name of a space from the shared items seen so far. */
export function knownSpaceName(tenantId: string): string | undefined {
  return names.get(tenantId)
}

/** Looks the space up in the first page of "Shared with me" when its name is not known yet. */
export async function findSpaceName(tenantId: string): Promise<string | undefined> {
  const known = knownSpaceName(tenantId)
  if (known) return known
  const page = await unwrap(client.GET('/v1/shared-with-me', { params: { query: { limit: 100 } } }))
  rememberSpaceNames(page.items)
  return knownSpaceName(tenantId)
}

/** For tests: forgets the names. */
export function forgetSpaceNames(): void {
  names.clear()
}
