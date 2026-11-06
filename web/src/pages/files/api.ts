import { client, RuumApiError, unwrap, type Schemas } from '../../api/client'
import { browser } from '../../app/browser'
import type { UploadTarget } from '../../upload/upload'

export type Node = Schemas['Node']

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

/** Where a new file goes: the folder, with a new name when the name is taken. */
export function newFileTarget(folder: Node, file: File): UploadTarget {
  return { target: 'new_file', parent_id: folder.id, name: file.name, on_conflict: 'rename' }
}

/** A new version of a file. It fails with `version_mismatch` if someone else wrote one first. */
export function newVersionTarget(node: Node): UploadTarget {
  return { target: 'new_version', node_id: node.id, base_version: node.head_version }
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

/**
 * Opens a preview in a new tab: a content-domain URL with `disposition=inline`. The content domain
 * serves it in a strict sandbox, so the file never runs as the platform.
 */
export async function previewNode(node: Node): Promise<void> {
  // Open the tab now, while the click still counts as a user action (pop-up blockers), and point
  // it at the URL when it comes.
  const tab = browser.openBlank()
  try {
    const res = await unwrap(
      client.GET('/v1/nodes/{node_id}/content-url', {
        params: {
          path: { node_id: node.id },
          query: { disposition: 'inline' },
          header: { 'Ruum-Tenant': node.tenant_id },
        },
      }),
    )
    browser.navigateTab(tab, res.url)
  } catch (e) {
    tab?.close()
    throw e
  }
}
