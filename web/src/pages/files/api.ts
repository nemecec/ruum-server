import { client, RuumApiError, unwrap, type Schemas } from '../../api/client'
import { browser } from '../../app/browser'

export type Node = Schemas['Node']

/** The largest file `putContent` takes in one request (32 MiB). Larger files need chunked uploads. */
export const MAX_SIMPLE_UPLOAD = 32 * 1024 * 1024

/** True for the answers that mean "you cannot see this" (`403`, or `404` for a node you cannot view). */
export function isNoAccess(e: unknown): boolean {
  return e instanceof RuumApiError && (e.status === 403 || e.status === 404)
}

/**
 * The folder and its parents, highest first. The walk stops at the first parent the user cannot
 * see (a guest, or a folder shared inside a tree), so the chain starts at the highest visible one.
 */
export async function loadAncestors(folder: Node): Promise<Node[]> {
  const chain: Node[] = [folder]
  let cur = folder
  // A folder tree is not deep; the limit only stops a loop on bad data.
  for (let i = 0; i < 64 && cur.parent_id; i++) {
    try {
      cur = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: cur.parent_id } } }))
    } catch (e) {
      if (isNoAccess(e)) break
      throw e
    }
    chain.unshift(cur)
  }
  return chain
}

/**
 * A folder and its breadcrumbs. Without `folderId` it is the root of the tenant; when the user
 * cannot see the root (a guest), `folder` is `null` and the page shows what is shared instead.
 */
export async function loadFolder(folderId: string | undefined): Promise<{ folder: Node | null; ancestors: Node[] }> {
  let folder: Node
  if (folderId) {
    folder = await unwrap(client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: folderId } } }))
  } else {
    try {
      folder = await unwrap(client.GET('/v1/nodes/root'))
    } catch (e) {
      if (isNoAccess(e)) return { folder: null, ancestors: [] }
      throw e
    }
  }
  return { folder, ancestors: await loadAncestors(folder) }
}

/** One page of the children of a folder. */
export function listChildren(folderId: string, cursor: string | undefined, kind?: 'folder' | 'file') {
  return unwrap(
    client.GET('/v1/nodes/{node_id}/children', {
      params: { path: { node_id: folderId }, query: { ...(cursor ? { cursor } : {}), ...(kind ? { kind } : {}) } },
    }),
  )
}

/** One page of "shared with me", across all tenants (the call takes no `Ruum-Tenant` header). */
export function listSharedWithMe(cursor: string | undefined) {
  return unwrap(client.GET('/v1/shared-with-me', { params: { query: cursor ? { cursor } : {} } }))
}

/**
 * Uploads a small file: `createNode` makes the file node and `putContent` writes the first
 * version. Files over 32 MiB need the chunked upload (`createUpload`), which comes with wave B.
 */
export async function uploadFile(parentId: string, file: File): Promise<Node> {
  if (file.size > MAX_SIMPLE_UPLOAD) {
    throw new RuumApiError(413, {
      code: 'payload_too_large',
      message: `${file.name} is larger than 32 MiB.`,
      hint: 'Large uploads are not in the web UI yet; use the CLI or WebDAV.',
    })
  }
  const created = await unwrap(
    client.POST('/v1/nodes', { body: { parent_id: parentId, name: file.name, kind: 'file', on_conflict: 'rename' } }),
  )
  const written = await unwrap(
    client.PUT('/v1/nodes/{node_id}/content', {
      params: { path: { node_id: created.node.id } },
      body: file as unknown as string,
      bodySerializer: (b: unknown) => b as BodyInit,
      headers: { 'Content-Type': file.type || 'application/octet-stream' },
    }),
  )
  return written.node
}

/** Downloads from the content domain (`getContentUrl`), never from the API origin. */
export async function downloadNode(node: Node, version?: string): Promise<void> {
  const res = await unwrap(
    client.GET('/v1/nodes/{node_id}/content-url', {
      params: {
        path: { node_id: node.id },
        query: { disposition: 'attachment', ...(version ? { version } : {}) },
        // The node's own tenant: a shared node can be in another tenant than the page.
        header: { 'Ruum-Tenant': node.tenant_id },
      },
    }),
  )
  browser.download(res.url, node.name)
}
