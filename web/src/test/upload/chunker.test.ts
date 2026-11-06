// The browser must cut and hash files exactly like the server (Rust crates `fastcdc` and `blake3`).
// The vectors come from those crates (vectors.ts says how).

import { describe, expect, it } from 'vitest'
import { chunkFile } from '../../upload/chunker'
import { FastCdc } from '../../upload/fastcdc'
import { vectors } from './vectors'

/** xorshift32 bytes, the same as the Rust program that made the vectors. */
function testData(n: number, seed: number): Uint8Array {
  const out = new Uint8Array(n)
  let s = seed >>> 0
  for (let i = 0; i < n; i++) {
    s ^= s << 13
    s >>>= 0
    s ^= s >>> 17
    s ^= s << 5
    s >>>= 0
    out[i] = s & 0xff
  }
  return out
}

const small = { min_size: 16384, avg_size: 32768, max_size: 65536 }
const big = { min_size: 1048576, avg_size: 2097152, max_size: 4194304 }

async function check(data: Uint8Array, sizes: typeof small, expected: { content: string; chunks: (string | number)[][] }) {
  const result = await chunkFile(new Blob([data as BlobPart]), sizes)
  expect(result.contentHash).toBe(expected.content)
  expect(result.chunks.map((c) => [c.size, c.hash])).toEqual(expected.chunks)
  let offset = 0
  for (const c of result.chunks) {
    expect(c.offset).toBe(offset)
    offset += c.size
  }
  expect(offset).toBe(data.length)
}

describe('chunking like the server', () => {
  const smallData = testData(1_000_001, 0x12345678)

  it('cuts and hashes with the small parameter set', () => check(smallData, small, vectors.small))

  it('cuts and hashes with the default parameter set, with a long run of zeros', async () => {
    const data = testData(9_000_000, 0x9e3779b9)
    data.fill(0, 2_000_000, 7_000_000)
    await check(data, big, vectors.big)
  })

  it('makes one chunk of a file below the minimum size, and none of an empty file', async () => {
    await check(smallData.subarray(0, 1000), small, vectors.tiny)
    await check(smallData.subarray(0, 0), small, vectors.empty)
  })

  it('refuses odd or unordered sizes', () => {
    expect(() => new FastCdc({ min_size: 3, avg_size: 4, max_size: 8 })).toThrow()
    expect(() => new FastCdc({ min_size: 64, avg_size: 32, max_size: 128 })).toThrow()
  })
})
