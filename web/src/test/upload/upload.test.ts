// The chunked upload flow against the mock API (which checks each request against the contract).

import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { setTenant } from '../../api/client'
import { db, ids } from '../../mocks/db'
import { server } from '../../mocks/node'
import { blake3Hex, chunkFile } from '../../upload/chunker'
import { uploadBlob, type UploadProgress } from '../../upload/upload'
import { recordRequests } from '../requests'

/** Bytes that do not repeat, so FastCDC cuts them into several chunks. */
function bytes(n: number, seed = 1): Uint8Array {
  const out = new Uint8Array(n)
  let s = seed
  for (let i = 0; i < n; i++) {
    s = (s * 1103515245 + 12345) >>> 0
    out[i] = s >>> 24
  }
  return out
}

const small = { min_size: 16384, avg_size: 32768, max_size: 65536 }
const target = { target: 'new_file' as const, parent_id: ids.personalRoot, name: 'data.bin', on_conflict: 'rename' as const }

describe('uploadBlob', () => {
  it('sends only the chunks the server does not have, then completes', async () => {
    setTenant(ids.personal)
    const data = bytes(300_000)
    const file = new Blob([data as BlobPart])
    const { chunks } = await chunkFile(file, small)
    expect(chunks.length).toBeGreaterThan(3)
    // The tenant already stores the first chunk.
    db.chunks.set(`${ids.personal}:${chunks[0].hash}`, data.slice(0, chunks[0].size))
    const seen = recordRequests()
    const progress: UploadProgress[] = []
    const res = await uploadBlob(file, target, { onProgress: (p) => progress.push(p) })

    const puts = seen.filter((r) => r.method === 'PUT').map((r) => r.path.split('/').at(-1))
    expect(puts.sort()).toEqual(chunks.slice(1).map((c) => c.hash).sort())
    expect(res.node.name).toBe('data.bin')
    expect(res.version.size_bytes).toBe(300_000)
    expect(res.version.content_hash).toBe(blake3Hex(data))
    expect(progress.map((p) => p.phase)).toEqual(expect.arrayContaining(['hashing', 'uploading', 'completing']))
    const lastUpload = progress.filter((p) => p.phase === 'uploading').at(-1)!
    expect(lastUpload.done).toBe(lastUpload.total)
    expect(lastUpload.total).toBe(300_000 - chunks[0].size)
  })

  it('tries a chunk again after a server error', async () => {
    setTenant(ids.personal)
    let failures = 0
    server.use(
      http.put('/v1/uploads/:id/chunks/:hash', () => {
        if (failures++ === 0) return HttpResponse.json({ error: { code: 'unavailable', message: 'Try again.' } }, { status: 503 })
        return undefined
      }),
    )
    const res = await uploadBlob(new Blob(['retry me']), { ...target, name: 'retry.txt' })
    expect(failures).toBe(2)
    expect(db.contents.get(res.version.id)).toBe('retry me')
  })

  it('does not try again after a client error, such as 501 on a server without uploads', async () => {
    setTenant(ids.personal)
    server.use(
      http.get('/v1/uploads/params', () =>
        HttpResponse.json({ error: { code: 'not_implemented', message: 'getChunkingParams is not implemented yet.' } }, { status: 501 }),
      ),
    )
    await expect(uploadBlob(new Blob(['x']), target)).rejects.toMatchObject({ status: 501, code: 'not_implemented' })
  })

  it('aborts the server session when canceled', async () => {
    setTenant(ids.personal)
    const controller = new AbortController()
    server.use(
      http.put('/v1/uploads/:id/chunks/:hash', () => {
        controller.abort(new DOMException('Canceled.', 'AbortError'))
        return HttpResponse.json({ error: { code: 'unavailable', message: 'Try again.' } }, { status: 503 })
      }),
    )
    const seen = recordRequests()
    await expect(uploadBlob(new Blob(['cancel me']), target, { signal: controller.signal })).rejects.toThrow()
    await expect.poll(() => seen.some((r) => r.method === 'DELETE' && r.path.startsWith('/v1/uploads/'))).toBe(true)
    const id = seen.find((r) => r.method === 'DELETE')!.path.split('/').at(-1)!
    await expect.poll(() => db.uploads.get(id)?.status).toBe('aborted')
  })

  it('asks again and sends a chunk that is gone when complete answers chunk_missing', async () => {
    setTenant(ids.personal)
    const data = bytes(300_000, 7)
    const file = new Blob([data as BlobPart])
    const { chunks } = await chunkFile(file, small)
    const lost = chunks[1].hash
    let completes = 0
    server.use(
      http.post('/v1/uploads/:id/complete', () => {
        // The first time, a chunk is gone from the store (for example, collected after a break).
        if (completes++ === 0) db.chunks.delete(`${ids.personal}:${lost}`)
        return undefined
      }),
    )
    const seen = recordRequests()
    const res = await uploadBlob(file, { ...target, name: 'lost.bin' })

    expect(completes).toBe(2)
    expect(seen.filter((r) => r.method === 'GET' && /^\/v1\/uploads\/[^/?]+$/.test(r.path))).toHaveLength(1)
    const puts = seen.filter((r) => r.method === 'PUT').map((r) => r.path.split('/').at(-1))
    expect(puts.filter((h) => h === lost)).toHaveLength(2)
    expect(puts).toHaveLength(chunks.length + 1)
    expect(res.version.content_hash).toBe(blake3Hex(data))
  })

  it('finishes and does not abort the session when canceled after complete is sent', async () => {
    setTenant(ids.personal)
    const controller = new AbortController()
    server.use(
      http.post('/v1/uploads/:id/complete', () => {
        controller.abort(new DOMException('Canceled.', 'AbortError'))
        return undefined
      }),
    )
    const seen = recordRequests()
    const res = await uploadBlob(new Blob(['late cancel']), { ...target, name: 'late.txt' }, { signal: controller.signal })
    expect(controller.signal.aborted).toBe(true)
    expect(db.contents.get(res.version.id)).toBe('late cancel')
    expect(seen.some((r) => r.method === 'DELETE')).toBe(false)
  })

  it('uses the tenant of the target when given', async () => {
    const seen = recordRequests()
    await uploadBlob(new Blob(['team file']), { ...target, parent_id: ids.teamRoot, name: 'team.txt' }, { tenantId: ids.team })
    expect(seen.every((r) => r.headers.get('Ruum-Tenant') === ids.team)).toBe(true)
  })
})
