/**
 * The key set of the gateway's user-token keys (`<issuer>/.well-known/app-jwks.json`).
 *
 * Sources, in this order:
 * 1. a static key set given in code (tests, dev mode);
 * 2. the file `RUUM_APP_JWKS_FILE` (a proposal: the runtime may mount the key set as a file,
 *    because a sandboxed app has no route to the platform origin by default); read at start and
 *    again for an unknown `kid`;
 * 3. an HTTPS GET of `<RUUM_ISSUER>/.well-known/app-jwks.json`, through `HTTPS_PROXY` when set.
 *
 * Only Ed25519 keys with a `kid` are kept, at most {@link MAX_KEYS}. A refresh for an unknown
 * `kid` happens at most once per `minRefreshMs`, so a flood of made-up `kid`s cannot make the app
 * fetch in a loop.
 */

import { readFileSync } from "node:fs";
import { createPublicKey, type KeyObject } from "node:crypto";
import { parseStrictJson } from "./json.js";
import { httpsGet } from "./proxy.js";

/** The most keys kept from one key set. */
export const MAX_KEYS = 16;
/** The largest key set accepted, in bytes. */
export const MAX_JWKS_BYTES = 64 * 1024;
/** The default shortest time between two refreshes, in milliseconds. */
export const DEFAULT_MIN_REFRESH_MS = 30_000;

/** A JSON Web Key Set as the gateway publishes it. */
export interface Jwks {
  keys: Record<string, unknown>[];
}

/** Options of {@link KeySet}. */
export interface KeySetOptions {
  /** A fixed key set (tests, dev mode). It is never refreshed. */
  jwks?: Jwks;
  /** A file that holds the key set (`RUUM_APP_JWKS_FILE`). */
  file?: string | undefined;
  /** The URL of the key set (`<issuer>/.well-known/app-jwks.json`). Must be `https:`. */
  url?: string | undefined;
  /** The shortest time between two refreshes, in milliseconds. Default 30 s. */
  minRefreshMs?: number;
  /** Replaces the HTTPS GET (tests). Returns the body text. */
  fetchText?: (url: string) => Promise<string>;
  /** The clock in milliseconds (tests). */
  nowMs?: () => number;
}

/** Turns a key set into a map from `kid` to public key, keeping only usable Ed25519 keys. */
export function keysFromJwks(jwks: unknown): Map<string, KeyObject> {
  const out = new Map<string, KeyObject>();
  if (typeof jwks !== "object" || jwks === null) return out;
  const keys = (jwks as { keys?: unknown }).keys;
  if (!Array.isArray(keys)) return out;
  for (const k of keys) {
    if (out.size >= MAX_KEYS) break;
    if (typeof k !== "object" || k === null) continue;
    const j = k as Record<string, unknown>;
    if (j["kty"] !== "OKP" || j["crv"] !== "Ed25519") continue;
    if (typeof j["kid"] !== "string" || j["kid"] === "" || j["kid"].length > 128) continue;
    if (j["alg"] !== undefined && j["alg"] !== "EdDSA") continue;
    if (j["use"] !== undefined && j["use"] !== "sig") continue;
    if (typeof j["x"] !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(j["x"])) continue;
    if ("d" in j) continue; // never accept a private key as a verification key
    if (out.has(j["kid"])) continue;
    try {
      out.set(j["kid"], createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: j["x"] }, format: "jwk" }));
    } catch {
      // skip a key that does not load
    }
  }
  return out;
}

/** Holds the verification keys and refreshes them, with a bound and a rate limit. */
export class KeySet {
  private keys = new Map<string, KeyObject>();
  private lastRefreshMs = Number.NEGATIVE_INFINITY;
  private inflight: Promise<void> | null = null;
  private readonly minRefreshMs: number;
  private readonly nowMs: () => number;

  constructor(private readonly options: KeySetOptions) {
    this.minRefreshMs = options.minRefreshMs ?? DEFAULT_MIN_REFRESH_MS;
    this.nowMs = options.nowMs ?? Date.now;
    if (options.jwks) {
      this.keys = keysFromJwks(options.jwks);
    } else if (options.file) {
      // Read at start, so a missing or broken file shows at once (a start-up error).
      this.keys = keysFromJwks(this.readFile(options.file));
      this.lastRefreshMs = this.nowMs();
    } else if (!options.url) {
      throw new Error("a key set needs a static key set, RUUM_APP_JWKS_FILE or an https issuer");
    }
  }

  private readFile(file: string): unknown {
    const text = readFileSync(file, "utf8");
    if (Buffer.byteLength(text) > MAX_JWKS_BYTES) throw new Error("the key set file is too large");
    return parseStrictJson(text);
  }

  private async load(): Promise<void> {
    if (this.options.file) {
      this.keys = keysFromJwks(this.readFile(this.options.file));
      return;
    }
    const url = this.options.url as string;
    const text = this.options.fetchText
      ? await this.options.fetchText(url)
      : await httpsGet(url, { maxBytes: MAX_JWKS_BYTES, timeoutMs: 5000 });
    if (Buffer.byteLength(text) > MAX_JWKS_BYTES) throw new Error("the key set is too large");
    this.keys = keysFromJwks(parseStrictJson(text));
  }

  /** Refreshes now unless the last refresh was less than `minRefreshMs` ago. */
  private refresh(): Promise<void> {
    if (this.options.jwks) return Promise.resolve();
    if (this.inflight) return this.inflight;
    const now = this.nowMs();
    if (now - this.lastRefreshMs < this.minRefreshMs) return Promise.resolve();
    this.lastRefreshMs = now;
    this.inflight = this.load()
      .catch(() => {
        // Keep the old keys; the caller sees an unknown kid. Never log the key set.
      })
      .finally(() => {
        this.inflight = null;
      });
    return this.inflight;
  }

  /** The key of `kid`, refreshing the set at most once per interval for an unknown `kid`. */
  async get(kid: string): Promise<KeyObject | undefined> {
    const k = this.keys.get(kid);
    if (k) return k;
    await this.refresh();
    return this.keys.get(kid);
  }

  /** The number of keys held now. */
  get size(): number {
    return this.keys.size;
  }
}
