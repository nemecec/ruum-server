// Resumable, chunked uploads (`getChunkingParams`, `createUpload`, `putChunk`, `completeUpload`,
// `getUpload`, `abortUpload`). The browser cuts the file like the server, sends the list of chunk
// hashes, and then uploads only the chunks that the server does not have yet (dedup, design 5.3).

import { client, RuumApiError, unwrap, type Schemas } from '../api/client'
import { asCreateUploadBody, type UploadTarget } from '../api/uploads'
import { chunkFile, readBytes, type FileChunk } from './chunker'

export type { UploadTarget }

export type UploadPhase = 'hashing' | 'uploading' | 'completing'

/** Progress of one upload. `done` and `total` are bytes of the current phase. */
export type UploadProgress = { phase: UploadPhase; done: number; total: number }

export type UploadOptions = {
  /** The tenant of the target (the `Ruum-Tenant` header); the page's tenant when not given. */
  tenantId?: string
  onProgress?: (p: UploadProgress) => void
  signal?: AbortSignal
  /** How many chunks are sent at the same time. */
  parallel?: number
}

/** How often one chunk is tried before the upload fails. */
const CHUNK_TRIES = 3

/** True for failures that a new try may fix: network errors, `429` and `5xx` (not `501`). */
function retryable(e: unknown): boolean {
  if (e instanceof RuumApiError) return e.status === 429 || (e.status >= 500 && e.status !== 501)
  return e instanceof TypeError
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const t = setTimeout(resolve, ms)
    signal?.addEventListener(
      'abort',
      () => {
        clearTimeout(t)
        reject(signal.reason)
      },
      { once: true },
    )
  })
}

function tenantHeader(opts: UploadOptions): { 'Ruum-Tenant'?: string } {
  return opts.tenantId ? { 'Ruum-Tenant': opts.tenantId } : {}
}

/**
 * Uploads a file or a new version of a file and returns the node and its new version.
 * Throws a `RuumApiError` for API errors, or the abort reason when `signal` aborts (the server
 * session is then aborted too). After the complete request is sent, `signal` no longer stops the
 * upload: the server may already have written the file, so a cancel then would be a lie.
 */
export async function uploadBlob(file: Blob, target: UploadTarget, opts: UploadOptions = {}): Promise<Schemas['ContentWrite']> {
  const { signal } = opts
  const header = tenantHeader(opts)
  const contentType = file.type || undefined
  const report = (phase: UploadPhase, done: number, total: number) => opts.onProgress?.({ phase, done, total })

  report('hashing', 0, file.size)
  const params = await unwrap(
    client.GET('/v1/uploads/params', {
      params: { query: { size: file.size, ...(contentType ? { content_type: contentType } : {}) }, header },
      signal,
    }),
  )
  const chunked = await chunkFile(file, params, { signal, onProgress: (done) => report('hashing', done, file.size) })

  const upload = await unwrap(
    client.POST('/v1/uploads', {
      params: { header },
      body: asCreateUploadBody({
        ...target,
        size: chunked.size,
        content_hash: chunked.contentHash,
        chunking: params.id,
        chunks: chunked.chunks.map((c) => ({ hash: c.hash, size: c.size })),
        ...(contentType ? { content_type: contentType } : {}),
      }),
      signal,
    }),
  )

  try {
    await sendMissing(upload.id, upload.missing, chunked.chunks, file, opts)
    report('completing', 0, 1)
    signal?.throwIfAborted()
    let result: Schemas['ContentWrite']
    try {
      result = await complete(upload.id, opts)
    } catch (e) {
      // A chunk can be gone after a break (for example, collected). Ask again and send what is missing.
      if (!(e instanceof RuumApiError) || e.code !== 'chunk_missing') throw e
      const again = await unwrap(
        client.GET('/v1/uploads/{upload_id}', { params: { path: { upload_id: upload.id }, header }, signal }),
      )
      await sendMissing(upload.id, again.missing, chunked.chunks, file, opts)
      signal?.throwIfAborted()
      result = await complete(upload.id, opts)
    }
    report('completing', 1, 1)
    return result
  } catch (e) {
    if (signal?.aborted) {
      // Best effort: the server also lets the session expire after 24 hours.
      void client
        .DELETE('/v1/uploads/{upload_id}', { params: { path: { upload_id: upload.id }, header } })
        .catch(() => undefined)
    }
    throw e
  }
}

/** Sends `completeUpload` without the abort signal: once sent, the server may finish it anyway. */
function complete(uploadId: string, opts: UploadOptions) {
  return unwrap(
    client.POST('/v1/uploads/{upload_id}/complete', {
      params: { path: { upload_id: uploadId }, header: tenantHeader(opts) },
    }),
  )
}

/** Sends the chunks in `missing`, each hash once, a few at a time. */
async function sendMissing(uploadId: string, missing: string[], chunks: FileChunk[], file: Blob, opts: UploadOptions) {
  const want = new Set(missing)
  const todo: FileChunk[] = []
  for (const c of chunks) {
    if (want.delete(c.hash)) todo.push(c)
  }
  const total = todo.reduce((n, c) => n + c.size, 0)
  let done = 0
  opts.onProgress?.({ phase: 'uploading', done, total })
  let next = 0
  const worker = async () => {
    while (next < todo.length) {
      const c = todo[next++]
      await putChunk(uploadId, c, file, opts)
      done += c.size
      opts.onProgress?.({ phase: 'uploading', done, total })
    }
  }
  const parallel = Math.max(1, Math.min(opts.parallel ?? 3, todo.length))
  await Promise.all(Array.from({ length: parallel }, worker))
}

async function putChunk(uploadId: string, c: FileChunk, file: Blob, opts: UploadOptions) {
  const bytes = await readBytes(file, c.offset, c.offset + c.size)
  for (let attempt = 1; ; attempt++) {
    opts.signal?.throwIfAborted()
    try {
      await unwrap(
        client.PUT('/v1/uploads/{upload_id}/chunks/{hash}', {
          params: { path: { upload_id: uploadId, hash: c.hash }, header: tenantHeader(opts) },
          body: bytes as unknown as string,
          bodySerializer: (b: unknown) => b as BodyInit,
          headers: { 'Content-Type': 'application/octet-stream' },
          signal: opts.signal,
        }),
      )
      return
    } catch (e) {
      if (attempt >= CHUNK_TRIES || !retryable(e) || opts.signal?.aborted) throw e
      await sleep(500 * 2 ** (attempt - 1), opts.signal)
    }
  }
}
