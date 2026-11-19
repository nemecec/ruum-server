import { generateKeyPairSync, sign, type KeyObject } from "node:crypto";
import { jwksOf, publicJwk, signToken } from "../src/dev.js";
import { UserTokenVerifier, type VerifierConfig } from "../src/verify.js";

export const ISSUER = "https://ruum.example";
export const APP_A = "0190b6c4-a00d-7cc1-9a4b-00000000000a";
export const APP_B = "0190b6c4-a00d-7cc1-9a4b-00000000000b";
export const DEPLOY = "0190b6c4-a00d-7cc1-9a4b-0000000000d1";
export const LIVE_A = `app:${APP_A}`;
export const PREVIEW_A = `app:${APP_A}/preview/${DEPLOY}`;
export const NOW = 1_800_000_000;

export const key: KeyObject = generateKeyPairSync("ed25519").privateKey;
export const otherKey: KeyObject = generateKeyPairSync("ed25519").privateKey;
export const kid = publicJwk(key)["kid"] as string;

/** Valid live claims of app A at {@link NOW}. */
export function claims(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    iss: ISSUER,
    aud: LIVE_A,
    typ: "app_user",
    env: "live",
    sub: "pairwise:visitor1",
    tenant: "acme",
    roles: ["member"],
    node_roles: { db: "edit" },
    actor: "user",
    iat: NOW - 5,
    exp: NOW + 55,
    jti: "j-1",
    ...over,
  };
}

/** Claims without the named members. */
export function without(c: Record<string, unknown>, ...names: string[]): Record<string, unknown> {
  const out = { ...c };
  for (const n of names) delete out[n];
  return out;
}

/** A token of `c` signed by `k` (default: the key of the set) with the gateway's header. */
export function token(c: Record<string, unknown>, k: KeyObject = key, header?: Record<string, unknown>): string {
  return signToken(c, k, header);
}

export const b64 = (s: string | Buffer): string => Buffer.from(s).toString("base64url");

/** A token from raw header and payload text, signed by `k`. */
export function rawToken(headerText: string, payloadText: string, k: KeyObject = key): string {
  const input = `${b64(headerText)}.${b64(payloadText)}`;
  return `${input}.${sign(null, Buffer.from(input), k).toString("base64url")}`;
}

/** A verifier of the live app A (or `over`), with the test key set and the clock at {@link NOW}. */
export function verifier(over: Partial<VerifierConfig> = {}): UserTokenVerifier {
  return new UserTokenVerifier({
    issuer: ISSUER,
    audience: LIVE_A,
    env: "live",
    jwks: jwksOf(key),
    now: () => NOW,
    ...over,
  });
}

export function previewVerifier(over: Partial<VerifierConfig> = {}): UserTokenVerifier {
  return verifier({ audience: PREVIEW_A, env: "preview", ...over });
}
