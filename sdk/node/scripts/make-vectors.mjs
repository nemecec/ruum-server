// Writes testdata/user-tokens.json: user tokens in the gateway's format, signed with a fixed test
// key made from a phrase (Ed25519 signatures are deterministic, so the file is reproducible).
// crates/ruum-gateway/tests/sdk_vectors.rs signs the same claims with the gateway's own
// UserTokenSigner and compares; the SDK's tests verify the same file. Run: npm run vectors.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { privateKeyFromSeed, publicJwk, signToken } from "../packages/app/dist/src/dev.js";

const SEED_PHRASE = "ruum sdk test vectors, not a key";
const ISSUER = "https://ruum.example";
const APP = "0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7d";
const OTHER_APP = "0190b6c4-a00d-7cc1-9a4b-00000000000b";
const DEPLOYMENT = "0190b6c4-a00d-7cc1-9a4b-0000000000d1";
const NOW = 1762380000;

const key = privateKeyFromSeed(Buffer.from(SEED_PHRASE, "utf8").subarray(0, 32));
const kid = publicJwk(key).kid;
const live = `app:${APP}`;
const preview = `app:${APP}/preview/${DEPLOYMENT}`;

// The claims in the order of the gateway's UserClaims struct.
function claims(over = {}) {
  const c = {
    iss: ISSUER,
    aud: live,
    typ: "app_user",
    env: "live",
    sub: "pairwise:6f2c9a81d4",
    tenant: "acme",
    roles: ["member", "approver"],
    node_roles: { db: "edit" },
    actor: "user",
    iat: NOW - 10,
    exp: NOW + 50,
    jti: "vector-1",
  };
  return { ...c, ...over };
}

const vectors = [
  { name: "live", claims: claims({ jti: "v-live" }), live: "accept", preview: "wrong_audience" },
  { name: "preview", claims: claims({ aud: preview, env: "preview", jti: "v-preview" }), live: "wrong_audience", preview: "accept" },
  { name: "another app", claims: claims({ aud: `app:${OTHER_APP}`, jti: "v-other" }), live: "wrong_audience", preview: "wrong_audience" },
  { name: "live audience, preview env", claims: claims({ env: "preview", jti: "v-env" }), live: "wrong_env", preview: "wrong_audience" },
  { name: "expired", claims: claims({ iat: NOW - 120, exp: NOW - 60, jti: "v-expired" }), live: "expired", preview: "wrong_audience" },
  { name: "exp - iat = 61", claims: claims({ iat: NOW - 10, exp: NOW + 51, jti: "v-61" }), live: "lifetime_too_long", preview: "wrong_audience" },
  { name: "exp - iat = 60", claims: claims({ iat: NOW - 10, exp: NOW + 50, jti: "v-60" }), live: "accept", preview: "wrong_audience" },
  {
    name: "unknown actor and sub prefix, with on_behalf_of",
    claims: (() => {
      const c = claims({ sub: "remote:z9", actor: "robot", jti: "v-actor" });
      // on_behalf_of sits between actor and iat in the gateway's struct.
      const { iat, exp, jti, ...head } = c;
      return { ...head, on_behalf_of: "pairwise:aa11", iat, exp, jti };
    })(),
    live: "accept",
    preview: "wrong_audience",
  },
  {
    name: "extra claim",
    claims: { ...claims({ jti: "v-extra" }), x_later_step: { level: 2 } },
    gateway_signs: false,
    live: "accept",
    preview: "wrong_audience",
  },
];

const out = {
  about:
    "Ruum-User test vectors (card O-5). Made by sdk/node/scripts/make-vectors.mjs; checked by crates/ruum-gateway/tests/sdk_vectors.rs (the gateway signs the same claims and must give the same parts) and by the @ruum/app tests. The key is made from seed_phrase (its first 32 bytes as the Ed25519 seed): a test key, never a real one.",
  seed_phrase: SEED_PHRASE,
  issuer: ISSUER,
  kid,
  now: NOW,
  live: { audience: live, env: "live" },
  preview: { audience: preview, env: "preview" },
  vectors: vectors.map((v) => ({
    name: v.name,
    gateway_signs: v.gateway_signs ?? true,
    claims: v.claims,
    parts: signToken(v.claims, key).split("."),
    live: v.live,
    preview: v.preview,
  })),
};

const file = fileURLToPath(new URL("../testdata/user-tokens.json", import.meta.url));
writeFileSync(file, `${JSON.stringify(out, null, 2)}\n`);
console.log(`wrote ${file}`);
