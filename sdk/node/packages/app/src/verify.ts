/**
 * The check of the `Ruum-User` token (`contracts/apps/user-token.schema.json`, decision 0006-10,
 * decision 0006 G point 2).
 */

import { verify as edVerify } from "node:crypto";
import { DevKeyError, KeySet, isDevIssuer, type Jwks } from "./keys.js";
import { parseStrictJson, StrictJsonError } from "./json.js";

/** The longest life of a user token (`exp - iat`), in seconds (the contract). */
export const MAX_LIFETIME_SECONDS = 60;
/** How far `iat` may be in the future (clock skew between the gateway and the app), in seconds. */
export const CLOCK_SKEW_SECONDS = 5;
/** The longest token accepted, in characters. */
export const MAX_TOKEN_LENGTH = 8192;

/** The machine-readable reasons of a refused token. */
export type RefusalReason =
  | "missing"
  | "malformed"
  | "too_large"
  | "bad_header"
  | "alg_not_allowed"
  | "unknown_kid"
  | "bad_signature"
  | "duplicate_claim"
  | "wrong_issuer"
  | "wrong_audience"
  | "wrong_type"
  | "wrong_env"
  | "bad_time"
  | "expired"
  | "lifetime_too_long"
  | "issued_in_future"
  | "bad_claims";

/**
 * The one error type of a refused token. `reason` is machine-readable; the message never holds
 * the token, a key or a claim value.
 */
export class RuumTokenError extends Error {
  constructor(public readonly reason: RefusalReason) {
    super(`the Ruum-User token was refused: ${reason}`);
    this.name = "RuumTokenError";
  }
}

/** Thrown at start-up when the configuration is missing or wrong. */
export class RuumConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "RuumConfigError";
  }
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
export const KNOWN_ACTORS: readonly string[] = Object.freeze(["user", "agent", "app", "partner", "anonymous"]);

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
export function configFromEnv(env: NodeJS.ProcessEnv = process.env): VerifierConfig {
  const need = (k: string): string => {
    const v = env[k];
    if (v === undefined || v.trim() === "") {
      throw new RuumConfigError(`${k} is not set; the Ruum runtime sets it. The SDK never accepts every token.`);
    }
    return v.trim();
  };
  const file = env["RUUM_APP_JWKS_FILE"];
  return {
    issuer: need("RUUM_ISSUER"),
    audience: need("RUUM_AUDIENCE"),
    env: need("RUUM_ENV"),
    jwksFile: file && file.trim() !== "" ? file.trim() : undefined,
  };
}

const B64URL = /^[A-Za-z0-9_-]+$/;

/** Decodes canonical base64url without padding; `null` for anything else. */
function b64(part: string): Buffer | null {
  if (!B64URL.test(part)) return null;
  const buf = Buffer.from(part, "base64url");
  // Refuses non-canonical encodings (trailing bits set, impossible lengths).
  return buf.toString("base64url") === part ? buf : null;
}

function json(buf: Buffer, malformed: RefusalReason): Record<string, unknown> {
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(buf);
  } catch {
    throw new RuumTokenError(malformed);
  }
  let v: unknown;
  try {
    v = parseStrictJson(text);
  } catch (e) {
    if (e instanceof StrictJsonError && e.kind === "duplicate_key") {
      throw new RuumTokenError(malformed === "bad_header" ? "bad_header" : "duplicate_claim");
    }
    throw new RuumTokenError(malformed);
  }
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new RuumTokenError(malformed);
  return v as Record<string, unknown>;
}

const isString = (v: unknown): v is string => typeof v === "string";

function hasOwn(o: object, k: string): boolean {
  return Object.prototype.hasOwnProperty.call(o, k);
}

/** `roles` items, `on_behalf_of` and `chain` items (`user-token.schema.json`). */
const ROLE = /^[a-z][a-z0-9_]{0,31}$/;
const ON_BEHALF_OF = /^pairwise:[A-Za-z0-9_-]{1,64}$/;
const CHAIN_APP = /^app:[0-9a-f-]{36}$/;
/** The values of `node_roles` (`user-token.schema.json`). */
const NODE_RIGHTS: readonly string[] = ["view", "comment", "edit", "manage"];

/** Header members that point to keys elsewhere, or that this SDK cannot honour. Never followed. */
const REFUSED_HEADER_MEMBERS = ["jku", "jwk", "x5u", "x5c", "crit", "b64", "zip", "enc"];

/** Checks `Ruum-User` tokens for one app. Make one at start-up and keep it. */
export class UserTokenVerifier {
  private readonly keys: KeySet;
  private readonly now: () => number;
  // Copies of the checked config values. The verifier never reads the caller's config object
  // after the constructor: a later change to it must not move the issuer, audience or env past
  // the start-up checks (or past the dev-key decision). Kimi review of O-5, run 13, finding 3.
  private readonly issuer: string;
  private readonly audience: string;
  private readonly env: string;

  /**
   * @param config what the app expects; usually {@link configFromEnv}. The verifier copies the
   *   values it needs; a change to `config` after this call has no effect (make a new verifier).
   * @throws RuumConfigError for a missing value or a key set that cannot be loaded.
   */
  constructor(config: VerifierConfig) {
    for (const k of ["issuer", "audience", "env"] as const) {
      if (!isString(config[k]) || config[k].trim() === "") {
        throw new RuumConfigError(`the verifier needs ${k}`);
      }
    }
    if (!/^app:[0-9a-f-]{36}(\/preview\/[0-9a-f-]{36})?$/.test(config.audience)) {
      throw new RuumConfigError("RUUM_AUDIENCE must be app:<app-id> or app:<app-id>/preview/<deployment-id>");
    }
    if (config.env !== "live" && config.env !== "preview") {
      throw new RuumConfigError("RUUM_ENV must be live or preview");
    }
    if ((config.env === "preview") !== config.audience.includes("/preview/")) {
      throw new RuumConfigError("RUUM_ENV and RUUM_AUDIENCE disagree about preview");
    }
    const issuer = config.issuer.replace(/\/+$/, "");
    let url: string | undefined;
    if (!config.jwks && !config.jwksFile) {
      if (!issuer.startsWith("https://")) {
        throw new RuumConfigError("without RUUM_APP_JWKS_FILE the issuer must be https:// to fetch its key set");
      }
      url = `${issuer}/.well-known/app-jwks.json`;
    }
    try {
      this.keys = new KeySet({
        ...(config.jwks ? { jwks: config.jwks } : {}),
        file: config.jwksFile,
        url,
        ...(config.minRefreshMs !== undefined ? { minRefreshMs: config.minRefreshMs } : {}),
        ...(config.fetchText ? { fetchText: config.fetchText } : {}),
        allowDevKeys: isDevIssuer(issuer),
      });
    } catch (e) {
      if (e instanceof DevKeyError) {
        throw new RuumConfigError(
          "the key set holds a dev-mode key (ruum-dev-token), but RUUM_ISSUER is not http://localhost: never ship .ruum-dev/ or set RUUM_APP_JWKS_FILE to it in a deployment",
        );
      }
      throw new RuumConfigError(`the key set cannot be loaded (${e instanceof Error ? e.name : "error"})`);
    }
    this.now = config.now ?? (() => Math.floor(Date.now() / 1000));
    this.issuer = issuer;
    this.audience = config.audience;
    this.env = config.env;
  }

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
  async verify(token: unknown): Promise<RuumUser> {
    if (token === undefined || token === null || token === "") throw new RuumTokenError("missing");
    if (!isString(token)) throw new RuumTokenError("malformed");
    if (token.length > MAX_TOKEN_LENGTH) throw new RuumTokenError("too_large");
    const parts = token.split(".");
    if (parts.length !== 3) throw new RuumTokenError("malformed");
    const [h, p, s] = parts as [string, string, string];
    const hb = b64(h);
    const pb = b64(p);
    const sb = b64(s);
    if (!hb || !pb || !sb) throw new RuumTokenError("malformed");

    // Header.
    const header = json(hb, "bad_header");
    if (!hasOwn(header, "alg")) throw new RuumTokenError("alg_not_allowed");
    if (header["alg"] !== "EdDSA") throw new RuumTokenError("alg_not_allowed");
    for (const m of REFUSED_HEADER_MEMBERS) {
      if (hasOwn(header, m)) throw new RuumTokenError("bad_header");
    }
    if (hasOwn(header, "typ") && header["typ"] !== "JWT") throw new RuumTokenError("bad_header");
    const kid = header["kid"];
    if (!isString(kid) || kid === "" || kid.length > 128) throw new RuumTokenError("bad_header");

    // Signature (Ed25519 signatures are 64 bytes).
    if (sb.length !== 64) throw new RuumTokenError("bad_signature");
    const key = await this.keys.get(kid);
    if (!key) throw new RuumTokenError("unknown_kid");
    let ok = false;
    try {
      ok = edVerify(null, Buffer.from(`${h}.${p}`, "ascii"), key, sb);
    } catch {
      ok = false;
    }
    if (!ok) throw new RuumTokenError("bad_signature");

    // Claims.
    const c = json(pb, "malformed");
    if (c["iss"] !== this.issuer) throw new RuumTokenError("wrong_issuer");
    if (c["aud"] !== this.audience) throw new RuumTokenError("wrong_audience");
    if (c["typ"] !== "app_user") throw new RuumTokenError("wrong_type");
    if (c["env"] !== this.env) throw new RuumTokenError("wrong_env");
    const iat = c["iat"];
    const exp = c["exp"];
    if (typeof iat !== "number" || typeof exp !== "number" || !Number.isSafeInteger(iat) || !Number.isSafeInteger(exp)) {
      throw new RuumTokenError("bad_time");
    }
    const now = this.now();
    if (exp <= now) throw new RuumTokenError("expired");
    if (exp <= iat || exp - iat > MAX_LIFETIME_SECONDS) throw new RuumTokenError("lifetime_too_long");
    if (iat > now + CLOCK_SKEW_SECONDS) throw new RuumTokenError("issued_in_future");

    const sub = c["sub"];
    const tenant = c["tenant"];
    const actor = c["actor"];
    const roles = c["roles"];
    const nodeRoles = c["node_roles"];
    if (!isString(sub) || !/^[a-z][a-z_]{0,31}:[A-Za-z0-9_-]{1,64}$/.test(sub)) throw new RuumTokenError("bad_claims");
    if (!isString(tenant) || tenant === "") throw new RuumTokenError("bad_claims");
    if (!isString(actor) || !/^[a-z][a-z_]{0,31}$/.test(actor)) throw new RuumTokenError("bad_claims");
    if (!Array.isArray(roles) || !roles.every((r) => isString(r) && ROLE.test(r))) throw new RuumTokenError("bad_claims");
    if (typeof nodeRoles !== "object" || nodeRoles === null || Array.isArray(nodeRoles)) {
      throw new RuumTokenError("bad_claims");
    }
    const nr: Record<string, string> = {};
    for (const [k, v] of Object.entries(nodeRoles)) {
      if (!isString(v) || !NODE_RIGHTS.includes(v)) throw new RuumTokenError("bad_claims");
      // defineProperty keeps a binding named `__proto__` as an own member.
      Object.defineProperty(nr, k, { value: v, enumerable: true, writable: true, configurable: true });
    }
    if (!isString(c["jti"])) throw new RuumTokenError("bad_claims");
    for (const k of ["name", "email", "locale", "on_behalf_of"]) {
      if (hasOwn(c, k) && !isString(c[k])) throw new RuumTokenError("bad_claims");
    }
    if (hasOwn(c, "on_behalf_of") && !ON_BEHALF_OF.test(c["on_behalf_of"] as string)) throw new RuumTokenError("bad_claims");
    if (hasOwn(c, "chain")) {
      const chain = c["chain"];
      if (!Array.isArray(chain) || chain.length > 3 || !chain.every((a) => isString(a) && CHAIN_APP.test(a))) {
        throw new RuumTokenError("bad_claims");
      }
    }
    if (hasOwn(c, "scopes")) {
      const scopes = c["scopes"];
      if (!Array.isArray(scopes) || !scopes.every(isString)) throw new RuumTokenError("bad_claims");
    }

    const user: RuumUser = {
      sub,
      tenant,
      roles: [...roles],
      node_roles: nr,
      actor,
      actorKnown: KNOWN_ACTORS.includes(actor),
      anonymous: sub.startsWith("anonymous:"),
      claims: Object.freeze({ ...c }),
    };
    if (isString(c["name"])) user.name = c["name"];
    if (isString(c["email"])) user.email = c["email"];
    if (isString(c["locale"])) user.locale = c["locale"];
    if (isString(c["on_behalf_of"])) user.on_behalf_of = c["on_behalf_of"];
    return user;
  }
}

let defaultVerifier: UserTokenVerifier | null = null;
/** One verifier (and so one key set and one refresh rate limit) per config object. */
const verifiers = new WeakMap<VerifierConfig, UserTokenVerifier>();

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
export async function verifyUserToken(token: unknown, config?: VerifierConfig): Promise<RuumUser> {
  if (config) {
    let v = verifiers.get(config);
    if (!v) {
      v = new UserTokenVerifier(config);
      verifiers.set(config, v);
    }
    return v.verify(token);
  }
  defaultVerifier ??= new UserTokenVerifier(configFromEnv());
  return defaultVerifier.verify(token);
}
