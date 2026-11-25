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
import { createPublicKey } from "node:crypto";
import { parseStrictJson } from "./json.js";
import { httpsGet } from "./proxy.js";
/** The most keys kept from one key set. */
export const MAX_KEYS = 16;
/** The largest key set accepted, in bytes. */
export const MAX_JWKS_BYTES = 64 * 1024;
/** The default shortest time between two refreshes, in milliseconds. */
export const DEFAULT_MIN_REFRESH_MS = 30_000;
/**
 * The JWK member that marks a dev-mode key (`ruum-dev-token init` writes it). The gateway's keys
 * never have it. A verifier trusts a marked key only for a dev issuer ({@link isDevIssuer}).
 */
export const DEV_KEY_MARK = "ruum_dev";
/** Whether `issuer` is a local dev origin: `http://localhost` or `http://127.0.0.1`, optionally with a port. */
export function isDevIssuer(issuer) {
    return /^http:\/\/(localhost|127\.0\.0\.1)(:\d{1,5})?$/.test(issuer);
}
/** Whether a key set holds a key with the dev mark. */
export function hasDevKeys(jwks) {
    if (typeof jwks !== "object" || jwks === null)
        return false;
    const keys = jwks.keys;
    return Array.isArray(keys) && keys.some((k) => typeof k === "object" && k !== null && DEV_KEY_MARK in k);
}
/**
 * Turns a key set into a map from `kid` to public key, keeping only usable Ed25519 keys. Keys with
 * the dev mark ({@link DEV_KEY_MARK}) are dropped unless `allowDevKeys` is `true`.
 */
export function keysFromJwks(jwks, allowDevKeys = false) {
    const out = new Map();
    if (typeof jwks !== "object" || jwks === null)
        return out;
    const keys = jwks.keys;
    if (!Array.isArray(keys))
        return out;
    for (const k of keys) {
        if (out.size >= MAX_KEYS)
            break;
        if (typeof k !== "object" || k === null)
            continue;
        const j = k;
        if (j["kty"] !== "OKP" || j["crv"] !== "Ed25519")
            continue;
        if (typeof j["kid"] !== "string" || j["kid"] === "" || j["kid"].length > 128)
            continue;
        if (j["alg"] !== undefined && j["alg"] !== "EdDSA")
            continue;
        if (j["use"] !== undefined && j["use"] !== "sig")
            continue;
        if (typeof j["x"] !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(j["x"]))
            continue;
        if ("d" in j)
            continue; // never accept a private key as a verification key
        if (DEV_KEY_MARK in j && !allowDevKeys)
            continue; // a dev key never verifies for a real issuer
        if (out.has(j["kid"]))
            continue;
        try {
            out.set(j["kid"], createPublicKey({ key: { kty: "OKP", crv: "Ed25519", x: j["x"] }, format: "jwk" }));
        }
        catch {
            // skip a key that does not load
        }
    }
    return out;
}
/** Thrown when a start-up key set holds a dev key but the issuer is not a dev issuer. */
export class DevKeyError extends Error {
    constructor() {
        super("the key set holds a dev-mode key (ruum-dev-token), but RUUM_ISSUER is not a dev issuer");
        this.name = "DevKeyError";
    }
}
/** Holds the verification keys and refreshes them, with a bound and a rate limit. */
export class KeySet {
    options;
    keys = new Map();
    lastRefreshMs = Number.NEGATIVE_INFINITY;
    inflight = null;
    minRefreshMs;
    nowMs;
    allowDev;
    constructor(options) {
        this.options = options;
        this.minRefreshMs = options.minRefreshMs ?? DEFAULT_MIN_REFRESH_MS;
        this.nowMs = options.nowMs ?? Date.now;
        this.allowDev = options.allowDevKeys === true;
        if (options.jwks) {
            this.keys = this.startKeys(options.jwks);
        }
        else if (options.file) {
            // Read at start, so a missing or broken file shows at once (a start-up error).
            this.keys = this.startKeys(this.readFile(options.file));
            this.lastRefreshMs = this.nowMs();
        }
        else if (!options.url) {
            throw new Error("a key set needs a static key set, RUUM_APP_JWKS_FILE or an https issuer");
        }
    }
    /** The keys of a start-up key set; a dev key for a real issuer is a start-up error. */
    startKeys(jwks) {
        if (!this.allowDev && hasDevKeys(jwks)) {
            throw new DevKeyError();
        }
        return keysFromJwks(jwks, this.allowDev);
    }
    readFile(file) {
        const text = readFileSync(file, "utf8");
        if (Buffer.byteLength(text) > MAX_JWKS_BYTES)
            throw new Error("the key set file is too large");
        return parseStrictJson(text);
    }
    async load() {
        if (this.options.file) {
            this.keys = keysFromJwks(this.readFile(this.options.file), this.allowDev);
            return;
        }
        const url = this.options.url;
        const text = this.options.fetchText
            ? await this.options.fetchText(url)
            : await httpsGet(url, { maxBytes: MAX_JWKS_BYTES, timeoutMs: 5000 });
        if (Buffer.byteLength(text) > MAX_JWKS_BYTES)
            throw new Error("the key set is too large");
        this.keys = keysFromJwks(parseStrictJson(text), this.allowDev);
    }
    /** Refreshes now unless the last refresh was less than `minRefreshMs` ago. */
    refresh() {
        if (this.options.jwks)
            return Promise.resolve();
        if (this.inflight)
            return this.inflight;
        const now = this.nowMs();
        if (now - this.lastRefreshMs < this.minRefreshMs)
            return Promise.resolve();
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
    async get(kid) {
        const k = this.keys.get(kid);
        if (k)
            return k;
        await this.refresh();
        return this.keys.get(kid);
    }
    /** The number of keys held now. */
    get size() {
        return this.keys.size;
    }
}
