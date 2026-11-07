import { client, RuumApiError, unwrap, type Schemas } from '../../api/client'
import { browser } from '../../app/browser'
import { tenantPath } from '../../app/tenant'
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
      cur = await unwrap(
        client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: cur.parent_id }, header: { 'Ruum-Tenant': folder.tenant_id } } }),
      )
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

/**
 * The key under which the server compares names in a folder: Unicode NFC, case-insensitive
 * (`Plan.txt` and `plan.txt` clash).
 */
export function nameKey(name: string): string {
  return name.normalize('NFC').toLowerCase() // i18n-ignore: a key that matches the server, never shown
}

/**
 * The files in `folder` whose names clash with `names`, read from the server now and not from
 * the list on the screen, which can be a moment old. Reads pages until it found every name.
 */
export async function findFilesByName(folder: Node, names: string[]): Promise<Node[]> {
  const wanted = new Set(names.map(nameKey))
  const found: Node[] = []
  let cursor: string | undefined
  // A folder has a bounded number of pages; the limit only stops a loop on bad data.
  for (let i = 0; i < 1000 && wanted.size; i++) {
    const page = await unwrap(
      client.GET('/v1/nodes/{node_id}/children', {
        params: {
          path: { node_id: folder.id },
          query: { kind: 'file', ...(cursor ? { cursor } : {}) },
          header: { 'Ruum-Tenant': folder.tenant_id },
        },
      }),
    )
    for (const n of page.items) {
      if (wanted.delete(nameKey(n.name))) found.push(n)
    }
    if (!page.next_cursor) break
    cursor = page.next_cursor
  }
  return found
}

/** The node as the server has it now; the node itself when that read fails. */
export async function refreshNode(node: Node): Promise<Node> {
  try {
    return await unwrap(
      client.GET('/v1/nodes/{node_id}', { params: { path: { node_id: node.id }, header: { 'Ruum-Tenant': node.tenant_id } } }),
    )
  } catch {
    return node
  }
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

/** What a bulk action did, and how to undo it. */
export type Undoable = { done: Node[]; undo: () => Promise<void> }

/**
 * Moves nodes to the trash, one call each. Undo restores them (with a new name when the old one is
 * taken). If one call fails, the nodes moved before it stay in the trash and the error is thrown.
 */
export async function trashNodes(nodes: Node[]): Promise<Undoable> {
  const done: Node[] = []
  for (const n of nodes) {
    await unwrap(client.POST('/v1/nodes/{node_id}/trash', { params: { path: { node_id: n.id }, header: { 'Ruum-Tenant': n.tenant_id } } }))
    done.push(n)
  }
  return {
    done,
    undo: async () => {
      for (const n of done) {
        await unwrap(
          client.POST('/v1/nodes/{node_id}/restore', {
            params: { path: { node_id: n.id }, header: { 'Ruum-Tenant': n.tenant_id } },
            body: { on_conflict: 'rename' },
          }),
        )
      }
    },
  }
}

/** Moves nodes into `target`. Undo moves each one back to the folder it came from. */
export async function moveNodes(nodes: Node[], target: Node): Promise<Undoable> {
  const done: Node[] = []
  for (const n of nodes) {
    if (n.parent_id === target.id) continue
    await unwrap(
      client.POST('/v1/nodes/{node_id}/move', {
        params: { path: { node_id: n.id }, header: { 'Ruum-Tenant': n.tenant_id } },
        body: { parent_id: target.id, on_conflict: 'fail' },
      }),
    )
    done.push(n)
  }
  return {
    done,
    undo: async () => {
      for (const n of done) {
        if (!n.parent_id) continue
        await unwrap(
          client.POST('/v1/nodes/{node_id}/move', {
            params: { path: { node_id: n.id }, header: { 'Ruum-Tenant': n.tenant_id } },
            body: { parent_id: n.parent_id, on_conflict: 'rename' },
          }),
        )
      }
    },
  }
}

/** The page of a shared item: a folder opens in the file browser, a file in its own page. */
export function sharedItemPath(node: Node): string {
  return node.kind === 'folder' ? tenantPath(node.tenant_id, 'files', node.id) : tenantPath(node.tenant_id, 'item', node.id)
}
