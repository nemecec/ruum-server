/**
 * Dev mode: a local Ed25519 key, its key set in a file, and user tokens for a chosen audience,
 * env and roles, so an app runs and is tested on a laptop without the platform.
 *
 * - It refuses to run when `RUUM_ENV` is `live` or `NODE_ENV` is `production`.
 * - Its issuer is `http://localhost` (or `http://localhost:<port>` / `http://127.0.0.1...`); a real
 *   deployment's `RUUM_ISSUER` is an `https://` origin and never equals it.
 * - The key stays in the local folder (default `.ruum-dev/`, with a `.gitignore` of `*`), with
 *   file mode 0600. Nothing in it is meant to be committed.
 * - It never follows a symbolic link at the dev folder or at its files: a checkout could plant
 *   `.ruum-dev/key.pem -> ../key.pem` and so put the key (or overwrite a file) outside the folder.
 *   Limit: Node has no `openat`, so a process that swaps the folder for a link while `initDev`
 *   runs is not stopped (backlog S-77); such a process can already change the code that runs.
 *
 * The token format is the gateway's (`crates/ruum-gateway/src/token.rs`): the header
 * `{"typ":"JWT","alg":"EdDSA","kid":<RFC 7638 thumbprint>}` and the claims in the gateway's order.
 */
import { type KeyObject } from "node:crypto";
import { type Jwks } from "./keys.js";
/** The default dev folder. */
export declare const DEV_DIR = ".ruum-dev";
/** The default dev issuer. */
export declare const DEV_ISSUER = "http://localhost";
/** Thrown when dev mode must not run, or gets a bad value. */
export declare class DevModeError extends Error {
    constructor(message: string);
}
/** Throws unless dev mode may run here (not `RUUM_ENV=live`, not `NODE_ENV=production`). */
export declare function assertDevAllowed(env?: NodeJS.ProcessEnv): void;
/** Throws unless `issuer` is a local `http://localhost` or `http://127.0.0.1` origin. */
export declare function assertDevIssuer(issuer: string): void;
/** The private key of a 32-byte Ed25519 seed (the gateway's `from_seed`). */
export declare function privateKeyFromSeed(seed: Uint8Array): KeyObject;
/** The public JWK of a key, with `kid` = the RFC 7638 SHA-256 thumbprint, like the gateway's. */
export declare function publicJwk(key: KeyObject): Record<string, string>;
/** The key set of one key. */
export declare function jwksOf(key: KeyObject): Jwks;
/**
 * The key set of a dev key: the key carries the dev mark ({@link DEV_KEY_MARK}), so a verifier
 * with a real (`https://`) issuer refuses to start with it.
 */
export declare function devJwksOf(key: KeyObject): Jwks;
/**
 * Signs claims as a user token with the gateway's header. The claims are written in the order of
 * the object, as the gateway's serde struct does.
 */
export declare function signToken(claims: Record<string, unknown>, key: KeyObject, header?: Record<string, unknown>): string;
/** What a dev token says. */
export interface DevTokenInput {
    /** `app:<app-id>` or `app:<app-id>/preview/<deployment-id>`. */
    audience: string;
    /** `live` or `preview`. Default: `preview` when the audience is a preview, else `live`. */
    env?: string;
    roles?: string[];
    /** Default `pairwise:dev-user`. */
    sub?: string;
    /** Default `dev`. */
    tenant?: string;
    /** Default `user`. */
    actor?: string;
    nodeRoles?: Record<string, string>;
    /** Default {@link DEV_ISSUER}. */
    issuer?: string;
    /** Seconds, 1 to 60. Default 60. */
    lifetime?: number;
    /** Extra claims (tests of the open contract). */
    extra?: Record<string, unknown>;
    /** The clock in seconds (tests). */
    now?: number;
}
/** The claims of a dev token, in the gateway's order. */
export declare function devClaims(input: DevTokenInput): Record<string, unknown>;
/** The files of a dev folder. */
export interface DevFiles {
    dir: string;
    keyFile: string;
    jwksFile: string;
}
/** The file paths of the dev folder `dir`. */
export declare function devFiles(dir?: string): DevFiles;
/**
 * For tests only: sets the no-follow open flag, so a test on macOS or Linux can run the path of a
 * platform without `O_NOFOLLOW` (Windows). Returns the flag before. Not part of the SDK's API.
 */
export declare function setNoFollowForTests(flag: number): number;
/**
 * For tests only: a function called between the check of a dev file and its open, so a test can
 * swap the file for a link at that moment. `undefined` removes it. Not part of the SDK's API.
 */
export declare function setBeforeOpenForTests(fn: ((path: string) => void) | undefined): void;
/**
 * Makes the dev folder with a new key (or keeps the existing key) and writes its key set.
 * Returns the paths; point `RUUM_APP_JWKS_FILE` at `jwksFile`.
 *
 * It refuses ({@link DevModeError}) a dev folder or a dev file that is a symbolic link.
 *
 * @example
 * const { jwksFile } = initDev();
 */
export declare function initDev(dir?: string, env?: NodeJS.ProcessEnv): DevFiles;
/** The private key of the dev folder. It refuses a key file that is a symbolic link. */
export declare function loadDevKey(dir?: string): KeyObject;
/**
 * Mints a user token with the dev key of `dir`.
 *
 * @example
 * const token = mintDevToken({ audience: "app:0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7d", roles: ["member"] });
 * await fetch("http://localhost:8080/", { headers: { "Ruum-User": token } });
 */
export declare function mintDevToken(input: DevTokenInput, dir?: string, env?: NodeJS.ProcessEnv): string;
