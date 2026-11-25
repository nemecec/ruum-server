/**
 * The check of the `Ruum-User` token (`contracts/apps/user-token.schema.json`, decision 0006-10,
 * decision 0006 G point 2).
 */
import { type Jwks } from "./keys.js";
/** The longest life of a user token (`exp - iat`), in seconds (the contract). */
export declare const MAX_LIFETIME_SECONDS = 60;
/** How far `iat` may be in the future (clock skew between the gateway and the app), in seconds. */
export declare const CLOCK_SKEW_SECONDS = 5;
/** The longest token accepted, in characters. */
export declare const MAX_TOKEN_LENGTH = 8192;
/** The machine-readable reasons of a refused token. */
export type RefusalReason = "missing" | "malformed" | "too_large" | "bad_header" | "alg_not_allowed" | "unknown_kid" | "bad_signature" | "duplicate_claim" | "wrong_issuer" | "wrong_audience" | "wrong_type" | "wrong_env" | "bad_time" | "expired" | "lifetime_too_long" | "issued_in_future" | "bad_claims";
/**
 * The one error type of a refused token. `reason` is machine-readable; the message never holds
 * the token, a key or a claim value.
 */
export declare class RuumTokenError extends Error {
    readonly reason: RefusalReason;
    constructor(reason: RefusalReason);
}
/** Thrown at start-up when the configuration is missing or wrong. */
export declare class RuumConfigError extends Error {
    constructor(message: string);
}
/** The signed-in visitor (or calling app) that a verified token describes. */
export interface RuumUser {
    /** `pairwise:<id>`, `app:<id>`, `partner:<key-id>`, `anonymous:<visit>`, or a later prefix. */
    sub: string;
    /** The tenant slug. */
    tenant: string;
    /** The visitor's app roles. Empty for anonymous visitors and partners. */
    roles: string[];
    /** The visitor's rights on the bound nodes, by binding name (for UI decisions only). */
    node_roles: Record<string, string>;
    /** `user`, `agent`, `app`, `partner`, `anonymous`, or a later value. */
    actor: string;
    /** Whether `actor` is one of the values this SDK knows. Treat an unknown actor like `anonymous`. */
    actorKnown: boolean;
    /** Whether this is a visitor without sign-in (`sub` starts with `anonymous:`). */
    anonymous: boolean;
    name?: string;
    email?: string;
    locale?: string;
    /** The user an agent or a calling app acts for. */
    on_behalf_of?: string;
    /** All claims of the token, also the ones this SDK does not know. */
    claims: Readonly<Record<string, unknown>>;
}
/** The known values of `actor`. */
export declare const KNOWN_ACTORS: readonly string[];
/** What the app expects, and where its keys come from. */
export interface VerifierConfig {
    /** `RUUM_ISSUER`: the origin of the Ruum instance. */
    issuer: string;
    /** `RUUM_AUDIENCE`: `app:<app-id>` or `app:<app-id>/preview/<deployment-id>`. */
    audience: string;
    /** `RUUM_ENV`: `live` or `preview`. */
    env: string;
    /** A fixed key set (tests, dev mode). */
    jwks?: Jwks;
    /** `RUUM_APP_JWKS_FILE`: a file with the key set. */
    jwksFile?: string | undefined;
    /** The shortest time between two key-set refreshes, in milliseconds. */
    minRefreshMs?: number;
    /** Replaces the HTTPS GET of the key set (tests). */
    fetchText?: (url: string) => Promise<string>;
    /** The clock in seconds (tests). */
    now?: () => number;
}
/**
 * Reads the configuration from the environment: `RUUM_ISSUER`, `RUUM_AUDIENCE`, `RUUM_ENV` and,
 * if set, `RUUM_APP_JWKS_FILE`. A missing value is a start-up error, never "accept all".
 *
 * @example
 * const config = configFromEnv(); // throws RuumConfigError when RUUM_ISSUER is missing
 */
export declare function configFromEnv(env?: NodeJS.ProcessEnv): VerifierConfig;
/** Checks `Ruum-User` tokens for one app. Make one at start-up and keep it. */
export declare class UserTokenVerifier {
    private readonly keys;
    private readonly now;
    private readonly issuer;
    private readonly audience;
    private readonly env;
    /**
     * @param config what the app expects; usually {@link configFromEnv}. The verifier copies the
     *   values it needs; a change to `config` after this call has no effect (make a new verifier).
     * @throws RuumConfigError for a missing value or a key set that cannot be loaded.
     */
    constructor(config: VerifierConfig);
    /**
     * Verifies a `Ruum-User` token and returns the user, or throws {@link RuumTokenError}.
     *
     * Order of the checks: the JWT shape and size, the header (`alg: EdDSA` only, a `kid`, no
     * `jku`/`jwk`/`x5u`/`x5c`/`crit`), the signature, then `iss`, `aud` (exact), `typ`, `env`, numeric
     * `iat` and `exp`, `exp` after now, `exp - iat` from 1 to 60 s, `iat` at most 5 s in the future,
     * and the known claims against `user-token.schema.json` (`sub`, `tenant`, `actor`, `roles`,
     * `node_roles`, `jti`, and `name`, `email`, `locale`, `on_behalf_of`, `chain`, `scopes` when
     * present). Unknown claims, actors and `sub` prefixes are accepted.
     */
    verify(token: unknown): Promise<RuumUser>;
}
/**
 * Verifies a `Ruum-User` token with the configuration from the environment (made on the first
 * call), or with `config` when given. Returns the user or throws {@link RuumTokenError}.
 *
 * The verifier of a `config` object is made once and kept for that object, so pass the same
 * object every time (make it at start-up). The verifier copies the values at that first call: a
 * later change to the object has no effect; pass a new object for a new configuration. Better: make one `new UserTokenVerifier(configFromEnv())`
 * at start-up and call its `verify`.
 *
 * @example
 * import { verifyUserToken } from "@ruum/app";
 * const user = await verifyUserToken(req.headers["ruum-user"]);
 * if (!user.roles.includes("approver")) return forbidden();
 */
export declare function verifyUserToken(token: unknown, config?: VerifierConfig): Promise<RuumUser>;
