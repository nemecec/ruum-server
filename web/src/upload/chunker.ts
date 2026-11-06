// Cuts a file into chunks and hashes them, as `createUpload` wants them: FastCDC 2020 boundaries,
// a BLAKE3 hash per chunk and one of the whole content, all as 64 lowercase hex characters.
//
// The file is read in slices, so a large file never sits in memory as a whole. The work yields to
// the page between slices, so the page stays usable while a large file is hashed.

import { blake3 } from '@noble/hashes/blake3.js'
import { bytesToHex } from '@noble/hashes/utils.js'
import { FastCdc, type ChunkSizes } from './fastcdc'

/** One chunk of a file: where it is, how long it is, and its BLAKE3 hash. */
export type FileChunk = { offset: number; size: number; hash: string }

export type ChunkedFile = { size: number; contentHash: string; chunks: FileChunk[] }

/** How many bytes are read from the file at a time (at least the largest chunk is kept as well). */
const READ_SIZE = 8 * 1024 * 1024

/** Reads `[start, end)` of a blob. jsdom (the tests) has no `Blob.arrayBuffer`, so it uses `FileReader`. */
export async function readBytes(blob: Blob, start: number, end: number): Promise<Uint8Array> {
  const part = blob.slice(start, end)
  if (typeof part.arrayBuffer === 'function') return new Uint8Array(await part.arrayBuffer())
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(new Uint8Array(reader.result as ArrayBuffer))
    reader.onerror = () => reject(reader.error ?? new Error('The file cannot be read.'))
    reader.readAsArrayBuffer(part)
  })
}

/** The BLAKE3 hash of some bytes, as 64 lowercase hex characters. */
export function blake3Hex(bytes: Uint8Array): string {
  return bytesToHex(blake3(bytes))
}

/**
 * Chunks and hashes a file with the given sizes. `onProgress` gets the number of bytes done.
 * Throws an `AbortError` when `signal` aborts.
 */
export async function chunkFile(
  file: Blob,
  sizes: ChunkSizes,
  opts: { onProgress?: (done: number) => void; signal?: AbortSignal } = {},
): Promise<ChunkedFile> {
  const cdc = new FastCdc(sizes)
  const whole = blake3.create()
  const chunks: FileChunk[] = []
  // `buf[pos..]` holds the bytes read but not yet cut; `bufOffset` is the file offset of `buf[0]`.
  let buf: Uint8Array = new Uint8Array(0)
  let pos = 0
  let bufOffset = 0
  let readTo = 0
  while (true) {
    opts.signal?.throwIfAborted()
    const eof = readTo >= file.size
    if (!eof && buf.length - pos < cdc.maxSize) {
      const next = await readBytes(file, readTo, Math.min(file.size, readTo + Math.max(READ_SIZE, cdc.maxSize)))
      readTo += next.length
      whole.update(next)
      const merged = new Uint8Array(buf.length - pos + next.length)
      merged.set(buf.subarray(pos), 0)
      merged.set(next, buf.length - pos)
      bufOffset += pos
      buf = merged
      pos = 0
      continue
    }
    if (pos >= buf.length) break
    const len = cdc.cut(buf, pos, buf.length)
    chunks.push({ offset: bufOffset + pos, size: len, hash: blake3Hex(buf.subarray(pos, pos + len)) })
    pos += len
    opts.onProgress?.(bufOffset + pos)
    // Let the page breathe between chunks of a large file.
    if (chunks.length % 8 === 0) await new Promise((r) => setTimeout(r, 0))
  }
  return { size: file.size, contentHash: bytesToHex(whole.digest()), chunks }
}
