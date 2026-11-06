import { client, unwrap } from '../../api/client'
import type { Node } from './api'

/** The coalescing window of the server (design 5.3): saves by one person within it join one version. */
export const COALESCE_MS = 10 * 60 * 1000

/**
 * True when a new version of `node` by `userId` would join the head version and replace its
 * content: the head is the person's own unnamed save of the last 10 minutes. Without the head
 * version the answer is false (the warning is not sure).
 */
export async function replacesOwnSave(node: Node, userId: string): Promise<boolean> {
  const mine = node.updated_by.actor.kind === 'user' && node.updated_by.actor.id === userId
  if (!mine || !node.head_version || Date.now() - new Date(node.updated_at).getTime() >= COALESCE_MS) return false
  try {
    const head = await unwrap(
      client.GET('/v1/nodes/{node_id}/versions/{version_id}', { params: { path: { node_id: node.id, version_id: node.head_version } } }),
    )
    return !head.label && !head.restored_from && head.actor.id === userId
  } catch {
    return false
  }
}

/** Names the head version of `node`, so a new version does not join it (named versions never join). */
export async function nameHeadVersion(node: Node, label: string): Promise<void> {
  if (!node.head_version) return
  await unwrap(
    client.PUT('/v1/nodes/{node_id}/versions/{version_id}/label', {
      params: { path: { node_id: node.id, version_id: node.head_version } },
      body: { label },
    }),
  )
}
