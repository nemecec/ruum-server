// FastCDC 2020 (content-defined chunking), a port of `fastcdc::v2020::FastCDC::new` of the Rust crate
// `fastcdc` 5.0.0, which the server uses (design section 5.3). The browser must cut a file at the same
// places as the server, so that equal content gives equal chunks and the server can deduplicate them.
//
// The gear hash is 64 bits. JavaScript numbers are not, so each 64-bit value is kept as two unsigned
// 32-bit halves (`hi`, `lo`). BigInt would be simpler but far too slow for large files.

import { GEAR_WORDS } from './gear'

/**
 * The cut-point masks of the crate (`v2020::MASKS`), indexed by the rounded base-2 logarithm of the
 * average chunk size, as `[hi, lo]` pairs. Indexes 0 to 4 are padding.
 */
// prettier-ignore
const MASKS: readonly (readonly [number, number])[] = [
  [0, 0], [0, 0], [0, 0], [0, 0], [0, 0],
  [0x00000000, 0x01804110], // unused except for normalization level 3
  [0x00000000, 0x01803110], // 64 B
  [0x00000000, 0x18035100], // 128 B
  [0x00000018, 0x00035300], // 256 B
  [0x00000190, 0x00353000], // 512 B
  [0x00005900, 0x03530000], // 1 KiB
  [0x0000d900, 0x03530000], // 2 KiB
  [0x0000d901, 0x03530000], // 4 KiB
  [0x0000d903, 0x03530000], // 8 KiB
  [0x0000d903, 0x13530000], // 16 KiB
  [0x0000d90f, 0x03530000], // 32 KiB
  [0x0000d903, 0x03537000], // 64 KiB
  [0x0000d907, 0x03537000], // 128 KiB
  [0x0000d907, 0x07537000], // 256 KiB
  [0x0000d917, 0x07537000], // 512 KiB
  [0x0000d917, 0x47537000], // 1 MiB
  [0x0000d917, 0x67537000], // 2 MiB
  [0x0000d937, 0x67537000], // 4 MiB
  [0x0000d937, 0x77537000], // 8 MiB
  [0x0000d937, 0x77577000], // 16 MiB
  [0x0000db37, 0x77577000], // unused except for normalization level 3
]

/** The normalization level of `FastCDC::new` (level 1). */
const NORMALIZATION = 1

const GEAR_HI = new Uint32Array(256)
const GEAR_LO = new Uint32Array(256)
// GEAR_LS of the crate: each GEAR value shifted left by one bit (modulo 2^64).
const GEAR_LS_HI = new Uint32Array(256)
const GEAR_LS_LO = new Uint32Array(256)
for (let i = 0; i < 256; i++) {
  const hi = GEAR_WORDS[2 * i] >>> 0
  const lo = GEAR_WORDS[2 * i + 1] >>> 0
  GEAR_HI[i] = hi
  GEAR_LO[i] = lo
  GEAR_LS_HI[i] = ((hi << 1) | (lo >>> 31)) >>> 0
  GEAR_LS_LO[i] = (lo << 1) >>> 0
}

/** The sizes of a chunking parameter set (`ChunkingParams` of the contract). */
export type ChunkSizes = { min_size: number; avg_size: number; max_size: number }

/** A chunker for one parameter set. `cut` finds the end of the next chunk. */
export class FastCdc {
  private readonly min: number
  private readonly avg: number
  private readonly max: number
  private readonly maskSHi: number
  private readonly maskSLo: number
  private readonly maskLHi: number
  private readonly maskLLo: number
  private readonly maskSLsHi: number
  private readonly maskSLsLo: number
  private readonly maskLLsHi: number
  private readonly maskLLsLo: number

  constructor(sizes: ChunkSizes) {
    const { min_size, avg_size, max_size } = sizes
    if (!(min_size > 0 && min_size <= avg_size && avg_size <= max_size) || min_size % 2 || avg_size % 2 || max_size % 2) {
      throw new Error(`Bad chunking sizes ${min_size}/${avg_size}/${max_size}.`)
    }
    this.min = min_size
    this.avg = avg_size
    this.max = max_size
    const bits = Math.round(Math.log2(avg_size))
    const s = MASKS[bits + NORMALIZATION]
    const l = MASKS[bits - NORMALIZATION]
    if (!s || !l) throw new Error(`No FastCDC mask for an average chunk size of ${avg_size}.`)
    ;[this.maskSHi, this.maskSLo] = s
    ;[this.maskLHi, this.maskLLo] = l
    this.maskSLsHi = ((s[0] << 1) | (s[1] >>> 31)) >>> 0
    this.maskSLsLo = (s[1] << 1) >>> 0
    this.maskLLsHi = ((l[0] << 1) | (l[1] >>> 31)) >>> 0
    this.maskLLsLo = (l[1] << 1) >>> 0
  }

  /** The largest chunk. `cut` needs this many bytes (or the end of the file) to decide. */
  get maxSize(): number {
    return this.max
  }

  /**
   * The length of the next chunk at the start of `src` (`v2020::cut_gear`). Give at least `maxSize`
   * bytes, or all the bytes left in the file.
   */
  cut(src: Uint8Array, start = 0, end = src.length): number {
    let remaining = end - start
    if (remaining <= this.min) return remaining
    let center = this.avg
    if (remaining > this.max) remaining = this.max
    else if (remaining < center) center = remaining
    const limit1 = center >>> 1
    const limit2 = remaining >>> 1
    let index = this.min >>> 1
    let hi = 0
    let lo = 0
    let g = 0
    let sum = 0
    // Strict mask below the average size, so chunks are less often small.
    while (index < limit1) {
      const a = start + index * 2
      g = src[a]
      // hash = (hash << 2) + GEAR_LS[byte]
      hi = ((hi << 2) | (lo >>> 30)) >>> 0
      lo = (lo << 2) >>> 0
      sum = lo + GEAR_LS_LO[g]
      hi = (hi + GEAR_LS_HI[g] + (sum > 0xffffffff ? 1 : 0)) >>> 0
      lo = sum >>> 0
      if ((hi & this.maskSLsHi) === 0 && (lo & this.maskSLsLo) === 0) return a - start
      // hash = hash + GEAR[next byte]
      g = src[a + 1]
      sum = lo + GEAR_LO[g]
      hi = (hi + GEAR_HI[g] + (sum > 0xffffffff ? 1 : 0)) >>> 0
      lo = sum >>> 0
      if ((hi & this.maskSHi) === 0 && (lo & this.maskSLo) === 0) return a + 1 - start
      index++
    }
    // Loose mask above it, so chunks are less often large.
    while (index < limit2) {
      const a = start + index * 2
      g = src[a]
      hi = ((hi << 2) | (lo >>> 30)) >>> 0
      lo = (lo << 2) >>> 0
      sum = lo + GEAR_LS_LO[g]
      hi = (hi + GEAR_LS_HI[g] + (sum > 0xffffffff ? 1 : 0)) >>> 0
      lo = sum >>> 0
      if ((hi & this.maskLLsHi) === 0 && (lo & this.maskLLsLo) === 0) return a - start
      g = src[a + 1]
      sum = lo + GEAR_LO[g]
      hi = (hi + GEAR_HI[g] + (sum > 0xffffffff ? 1 : 0)) >>> 0
      lo = sum >>> 0
      if ((hi & this.maskLHi) === 0 && (lo & this.maskLLo) === 0) return a + 1 - start
      index++
    }
    return remaining
  }
}
