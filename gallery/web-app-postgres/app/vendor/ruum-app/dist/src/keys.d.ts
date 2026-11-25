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
import { type KeyObject } from "node:crypto";
/** The most keys kept from one key set. */
export declare const MAX_KEYS = 16;
/** The largest key set accepted, in bytes. */
export declare const MAX_JWKS_BYTES: number;
/** The default shortest time between two refreshes, in milliseconds. */
export declare const DEFAULT_MIN_REFRESH_MS = 30000;
/**
 * The JWK member that marks a dev-mode key (`ruum-dev-token init` writes it). The gateway's keys
 * never have it. A verifier trusts a marked key only for a dev issuer ({@link isDevIssuer}).
 */
export declare const DEV_KEY_MARK = "ruum_dev";
/** Whether `issuer` is a local dev origin: `http://localhost` or `http://127.0.0.1`, optionally with a port. */
export declare function isDevIssuer(issuer: string): boolean;
/** Whether a key set holds a key with the dev mark. */
export declare function hasDevKeys(jwks: unknown): boolean;
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
    /**
     * Whether keys with the dev mark are trusted (only for a dev issuer). Default `false`: a static
     * key set or a key file with a marked key is a start-up error, and a refresh drops marked keys.
     */
    allowDevKeys?: boolean;
}
/**
 * Turns a key set into a map from `kid` to public key, keeping only usable Ed25519 keys. Keys with
 * the dev mark ({@link DEV_KEY_MARK}) are dropped unless `allowDevKeys` is `true`.
 */
export declare function keysFromJwks(jwks: unknown, allowDevKeys?: boolean): Map<string, KeyObject>;
/** Thrown when a start-up key set holds a dev key but the issuer is not a dev issuer. */
export declare class DevKeyError extends Error {
    constructor();
}
/** Holds the verification keys and refreshes them, with a bound and a rate limit. */
export declare class KeySet {
    private readonly options;
    private keys;
    private lastRefreshMs;
    private inflight;
    private readonly minRefreshMs;
    private readonly nowMs;
    private readonly allowDev;
    constructor(options: KeySetOptions);
    /** The keys of a start-up key set; a dev key for a real issuer is a start-up error. */
    private startKeys;
    private readFile;
    private load;
    /** Refreshes now unless the last refresh was less than `minRefreshMs` ago. */
    private refresh;
    /** The key of `kid`, refreshing the set at most once per interval for an unknown `kid`. */
    get(kid: string): Promise<KeyObject | undefined>;
    /** The number of keys held now. */
    get size(): number;
}
