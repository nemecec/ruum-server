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
 *   runs is not stopped (backlog S-76); such a process can already change the code that runs.
 *
 * The token format is the gateway's (`crates/ruum-gateway/src/token.rs`): the header
 * `{"typ":"JWT","alg":"EdDSA","kid":<RFC 7638 thumbprint>}` and the claims in the gateway's order.
 */

import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, randomUUID, sign, type KeyObject } from "node:crypto";
import { closeSync, constants, fchmodSync, fstatSync, lstatSync, mkdirSync, openSync, readFileSync, writeFileSync, type Stats } from "node:fs";
import { join, resolve } from "node:path";
import { DEV_KEY_MARK, isDevIssuer, type Jwks } from "./keys.js";
import { MAX_LIFETIME_SECONDS } from "./verify.js";

/** The default dev folder. */
export const DEV_DIR = ".ruum-dev";
/** The default dev issuer. */
export const DEV_ISSUER = "http://localhost";

/** Thrown when dev mode must not run, or gets a bad value. */
export class DevModeError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DevModeError";
  }
}

/** Throws unless dev mode may run here (not `RUUM_ENV=live`, not `NODE_ENV=production`). */
export function assertDevAllowed(env: NodeJS.ProcessEnv = process.env): void {
  // Trimmed and case-folded, like configFromEnv trims: " live " is live.
  if (env["RUUM_ENV"]?.trim().toLowerCase() === "live") throw new DevModeError("dev mode refuses to run with RUUM_ENV=live");
  if (env["NODE_ENV"]?.trim().toLowerCase() === "production") {
    throw new DevModeError("dev mode refuses to run with NODE_ENV=production");
  }
}

/** Throws unless `issuer` is a local `http://localhost` or `http://127.0.0.1` origin. */
export function assertDevIssuer(issuer: string): void {
  if (!isDevIssuer(issuer)) {
    throw new DevModeError("a dev issuer must be http://localhost or http://127.0.0.1, optionally with a port");
  }
}

const ED25519_PKCS8_PREFIX = Buffer.from("302e020100300506032b657004220420", "hex");

/** The private key of a 32-byte Ed25519 seed (the gateway's `from_seed`). */
export function privateKeyFromSeed(seed: Uint8Array): KeyObject {
  if (seed.length !== 32) throw new DevModeError("an Ed25519 seed has 32 bytes");
  return createPrivateKey({ key: Buffer.concat([ED25519_PKCS8_PREFIX, Buffer.from(seed)]), format: "der", type: "pkcs8" });
}

/** The public JWK of a key, with `kid` = the RFC 7638 SHA-256 thumbprint, like the gateway's. */
export function publicJwk(key: KeyObject): Record<string, string> {
  const pub = key.type === "private" ? createPublicKey(key) : key;
  const x = (pub.export({ format: "jwk" }) as { x?: string }).x;
  if (!x) throw new DevModeError("not an Ed25519 key");
  const kid = createHash("sha256").update(`{"crv":"Ed25519","kty":"OKP","x":"${x}"}`).digest("base64url");
  return { use: "sig", alg: "EdDSA", kid, kty: "OKP", crv: "Ed25519", x };
}

/** The key set of one key. */
export function jwksOf(key: KeyObject): Jwks {
  return { keys: [publicJwk(key)] };
}

/**
 * The key set of a dev key: the key carries the dev mark ({@link DEV_KEY_MARK}), so a verifier
 * with a real (`https://`) issuer refuses to start with it.
 */
export function devJwksOf(key: KeyObject): Jwks {
  return { keys: [{ ...publicJwk(key), [DEV_KEY_MARK]: true }] };
}

/**
 * Signs claims as a user token with the gateway's header. The claims are written in the order of
 * the object, as the gateway's serde struct does.
 */
export function signToken(claims: Record<string, unknown>, key: KeyObject, header?: Record<string, unknown>): string {
  const h = header ?? { typ: "JWT", alg: "EdDSA", kid: publicJwk(key)["kid"] };
  const enc = (v: unknown) => Buffer.from(JSON.stringify(v), "utf8").toString("base64url");
  const input = `${enc(h)}.${enc(claims)}`;
  return `${input}.${sign(null, Buffer.from(input, "ascii"), key).toString("base64url")}`;
}

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
export function devClaims(input: DevTokenInput): Record<string, unknown> {
  const issuer = input.issuer ?? DEV_ISSUER;
  assertDevIssuer(issuer);
  if (!/^app:[0-9a-f-]{36}(\/preview\/[0-9a-f-]{36})?$/.test(input.audience)) {
    throw new DevModeError("the audience must be app:<app-id> or app:<app-id>/preview/<deployment-id>");
  }
  const lifetime = input.lifetime ?? MAX_LIFETIME_SECONDS;
  if (!Number.isInteger(lifetime) || lifetime < 1 || lifetime > MAX_LIFETIME_SECONDS) {
    throw new DevModeError("the lifetime must be 1 to 60 seconds");
  }
  const now = input.now ?? Math.floor(Date.now() / 1000);
  return {
    iss: issuer,
    aud: input.audience,
    typ: "app_user",
    env: input.env ?? (input.audience.includes("/preview/") ? "preview" : "live"),
    sub: input.sub ?? "pairwise:dev-user",
    tenant: input.tenant ?? "dev",
    roles: input.roles ?? [],
    node_roles: input.nodeRoles ?? {},
    actor: input.actor ?? "user",
    iat: now,
    exp: now + lifetime,
    jti: randomUUID(),
    ...(input.extra ?? {}),
  };
}

/** The files of a dev folder. */
export interface DevFiles {
  dir: string;
  keyFile: string;
  jwksFile: string;
}

/** The file paths of the dev folder `dir`. */
export function devFiles(dir: string = DEV_DIR): DevFiles {
  const d = resolve(dir);
  return { dir: d, keyFile: join(d, "key.pem"), jwksFile: join(d, "jwks.json") };
}

// O_NOFOLLOW is 0 where the platform has none (Windows): there only the lstat check in openPlain
// refuses a link (Kimi re-check of O-5, run 13: loadDevKey had no lstat and followed a key link).
let NOFOLLOW = constants.O_NOFOLLOW ?? 0;

/**
 * For tests only: sets the no-follow open flag, so a test on macOS or Linux can run the path of a
 * platform without `O_NOFOLLOW` (Windows). Returns the flag before. Not part of the SDK's API.
 */
export function setNoFollowForTests(flag: number): number {
  const before = NOFOLLOW;
  NOFOLLOW = flag;
  return before;
}

/** lstat, or undefined when nothing is at `path`. */
function lstatOrNone(path: string): Stats | undefined {
  try {
    return lstatSync(path);
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw e;
  }
}

function refuseLink(path: string): never {
  throw new DevModeError(`dev mode refuses a symbolic link or a special file at ${path}`);
}

/**
 * Opens `path` without following a symbolic link at its last part, and checks that it is a plain
 * file. Kimi review of O-5 (run 13), finding 2.
 */
function openPlain(path: string, flags: number, mode?: number): number {
  // Every open checks first: without O_NOFOLLOW (Windows) this is what refuses a link.
  const st = lstatOrNone(path);
  if (st && !st.isFile()) refuseLink(path);
  let fd: number;
  try {
    fd = openSync(path, flags | NOFOLLOW, mode);
  } catch (e) {
    const code = (e as NodeJS.ErrnoException).code;
    // ELOOP: a symbolic link (O_NOFOLLOW). EEXIST: something appeared at a new key's path.
    if (code === "ELOOP" || code === "EMLINK" || code === "EEXIST") refuseLink(path);
    throw e;
  }
  if (!fstatSync(fd).isFile()) {
    closeSync(fd);
    refuseLink(path);
  }
  return fd;
}

/** Writes `data` to the plain file `path` (made when missing), never through a symbolic link. */
function writePlain(path: string, data: string, mode = 0o666): void {
  const fd = openPlain(path, constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC, mode);
  try {
    writeFileSync(fd, data);
  } finally {
    closeSync(fd);
  }
}

/**
 * Makes the dev folder with a new key (or keeps the existing key) and writes its key set.
 * Returns the paths; point `RUUM_APP_JWKS_FILE` at `jwksFile`.
 *
 * It refuses ({@link DevModeError}) a dev folder or a dev file that is a symbolic link.
 *
 * @example
 * const { jwksFile } = initDev();
 */
export function initDev(dir: string = DEV_DIR, env: NodeJS.ProcessEnv = process.env): DevFiles {
  assertDevAllowed(env);
  const f = devFiles(dir);
  const d = lstatOrNone(f.dir);
  if (d && !d.isDirectory()) refuseLink(f.dir);
  if (!d) mkdirSync(f.dir, { recursive: true, mode: 0o700 });
  if (!lstatSync(f.dir).isDirectory()) refuseLink(f.dir);
  writePlain(join(f.dir, ".gitignore"), "*\n");
  const k = lstatOrNone(f.keyFile);
  if (k && !k.isFile()) refuseLink(f.keyFile);
  if (!k) {
    const { privateKey } = generateKeyPairSync("ed25519");
    // O_EXCL: a new key never goes through something that appeared at the path.
    const fd = openPlain(f.keyFile, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, 0o600);
    try {
      writeFileSync(fd, privateKey.export({ format: "pem", type: "pkcs8" }));
      fchmodSync(fd, 0o600);
    } finally {
      closeSync(fd);
    }
  } else {
    const fd = openPlain(f.keyFile, constants.O_RDONLY);
    try {
      fchmodSync(fd, 0o600);
    } finally {
      closeSync(fd);
    }
  }
  writePlain(f.jwksFile, `${JSON.stringify(devJwksOf(loadDevKey(dir)), null, 2)}\n`);
  return f;
}

/** The private key of the dev folder. It refuses a key file that is a symbolic link. */
export function loadDevKey(dir: string = DEV_DIR): KeyObject {
  const f = devFiles(dir);
  const d = lstatOrNone(f.dir);
  if (d && !d.isDirectory()) refuseLink(f.dir);
  const fd = openPlain(f.keyFile, constants.O_RDONLY);
  let pem: string;
  try {
    pem = readFileSync(fd, "utf8");
  } finally {
    closeSync(fd);
  }
  const key = createPrivateKey(pem);
  if (key.asymmetricKeyType !== "ed25519") throw new DevModeError("the dev key is not Ed25519");
  return key;
}

/**
 * Mints a user token with the dev key of `dir`.
 *
 * @example
 * const token = mintDevToken({ audience: "app:0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7d", roles: ["member"] });
 * await fetch("http://localhost:8080/", { headers: { "Ruum-User": token } });
 */
export function mintDevToken(input: DevTokenInput, dir: string = DEV_DIR, env: NodeJS.ProcessEnv = process.env): string {
  assertDevAllowed(env);
  return signToken(devClaims(input), loadDevKey(dir));
}
