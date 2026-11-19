import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { DevModeError, assertDevAllowed, devClaims, initDev, jwksOf, mintDevToken, privateKeyFromSeed, publicJwk } from "../src/dev.js";
import { main } from "../src/cli.js";
import { RuumTokenError, UserTokenVerifier, type RefusalReason } from "../src/verify.js";
import { LIVE_A, PREVIEW_A } from "./helpers.js";

const DEV_ENV = { RUUM_ENV: "preview", NODE_ENV: "development" } as NodeJS.ProcessEnv;

function tmp(): { dir: string; done: () => void } {
  const base = mkdtempSync(join(tmpdir(), "ruum-o5-dev-"));
  return { dir: join(base, ".ruum-dev"), done: () => rmSync(base, { recursive: true, force: true }) };
}

test("dev mode refuses RUUM_ENV=live and NODE_ENV=production", () => {
  assert.throws(() => assertDevAllowed({ RUUM_ENV: "live" }), DevModeError);
  assert.throws(() => assertDevAllowed({ NODE_ENV: "production" }), DevModeError);
  assert.doesNotThrow(() => assertDevAllowed({ RUUM_ENV: "preview" }));
  const t = tmp();
  try {
    assert.throws(() => initDev(t.dir, { RUUM_ENV: "live" }), DevModeError);
    assert.equal(existsSync(t.dir), false, "nothing was written");
    initDev(t.dir, DEV_ENV);
    assert.throws(() => mintDevToken({ audience: LIVE_A }, t.dir, { NODE_ENV: "production" }), DevModeError);
  } finally {
    t.done();
  }
});

test("dev issuers are local http origins that a real RUUM_ISSUER never equals", () => {
  assert.equal(devClaims({ audience: LIVE_A })["iss"], "http://localhost");
  assert.equal(devClaims({ audience: LIVE_A, issuer: "http://localhost:8080" })["iss"], "http://localhost:8080");
  for (const issuer of ["https://ruum.space", "https://ruum.localhost", "http://ruum.example", "http://localhost.evil.example"]) {
    assert.throws(() => devClaims({ audience: LIVE_A, issuer }), DevModeError, issuer);
  }
  assert.throws(() => devClaims({ audience: LIVE_A, lifetime: 61 }), DevModeError);
  assert.throws(() => devClaims({ audience: "app:x" }), DevModeError);
});

test("init writes a private key (0600) and its JWKS; minted tokens verify locally and nowhere else", async () => {
  const t = tmp();
  try {
    const f = initDev(t.dir, DEV_ENV);
    assert.equal(readFileSync(join(f.dir, ".gitignore"), "utf8"), "*\n");
    assert.equal(statSync(f.keyFile).mode & 0o777, 0o600);
    const jwks = JSON.parse(readFileSync(f.jwksFile, "utf8"));
    assert.equal(jwks.keys.length, 1);
    assert.equal(jwks.keys[0].d, undefined, "the JWKS has no private part");
    // A second init keeps the key.
    initDev(t.dir, DEV_ENV);
    assert.deepEqual(JSON.parse(readFileSync(f.jwksFile, "utf8")), jwks);

    const tok = mintDevToken({ audience: PREVIEW_A, roles: ["member"] }, t.dir, DEV_ENV);
    const local = new UserTokenVerifier({ issuer: "http://localhost", audience: PREVIEW_A, env: "preview", jwksFile: f.jwksFile });
    const user = await local.verify(tok);
    assert.deepEqual(user.roles, ["member"]);
    // A real deployment (https issuer) refuses a dev token even if it had the dev key.
    const real = new UserTokenVerifier({ issuer: "https://ruum.space", audience: PREVIEW_A, env: "preview", jwksFile: f.jwksFile });
    await assert.rejects(real.verify(tok), (e) => (e as RuumTokenError).reason === "wrong_issuer");
  } finally {
    t.done();
  }
});

test("the ruum-dev-token CLI inits and mints", async () => {
  const t = tmp();
  const saved = { ...process.env };
  try {
    process.env["RUUM_ENV"] = "preview";
    delete process.env["NODE_ENV"];
    let out = "";
    let err = "";
    assert.equal(main(["init", "--dir", t.dir], (s) => (out += s), (s) => (err += s)), 0);
    assert.match(out, /RUUM_APP_JWKS_FILE=/);
    out = "";
    assert.equal(main(["mint", "--dir", t.dir, "--aud", PREVIEW_A, "--roles", "a,b"], (s) => (out += s), (s) => (err += s)), 0);
    const tok = out.trim();
    const v = new UserTokenVerifier({ issuer: "http://localhost", audience: PREVIEW_A, env: "preview", jwksFile: join(t.dir, "jwks.json") });
    assert.deepEqual((await v.verify(tok)).roles, ["a", "b"]);
    assert.equal(main(["mint", "--dir", t.dir], () => undefined, (s) => (err += s)), 1);
    assert.equal(main(["nope"], () => undefined, (s) => (err += s)), 2);
    process.env["RUUM_ENV"] = "live";
    err = "";
    assert.equal(main(["mint", "--dir", t.dir, "--aud", LIVE_A], () => undefined, (s) => (err += s)), 1);
    assert.match(err, /RUUM_ENV=live/);
  } finally {
    process.env = saved;
    t.done();
  }
});

// ----------------------------------------------------------------- the gateway's vectors

interface VectorFile {
  seed_phrase: string;
  issuer: string;
  kid: string;
  now: number;
  live: { audience: string; env: string };
  preview: { audience: string; env: string };
  vectors: { name: string; gateway_signs: boolean; claims: Record<string, unknown>; parts: string[]; live: string; preview: string }[];
}

function vectorFile(): VectorFile {
  let dir = dirname(fileURLToPath(import.meta.url));
  for (let i = 0; i < 8; i++) {
    const f = join(dir, "testdata", "user-tokens.json");
    if (existsSync(f)) return JSON.parse(readFileSync(f, "utf8")) as VectorFile;
    dir = dirname(dir);
  }
  throw new Error("sdk/node/testdata/user-tokens.json not found");
}

test("the gateway's token vectors (testdata/user-tokens.json) verify as the file says", async () => {
  const file = vectorFile();
  const k = privateKeyFromSeed(Buffer.from(file.seed_phrase, "utf8").subarray(0, 32));
  assert.equal(publicJwk(k)["kid"], file.kid);
  const make = (which: "live" | "preview") =>
    new UserTokenVerifier({ issuer: file.issuer, audience: file[which].audience, env: file[which].env, jwks: jwksOf(k), now: () => file.now });
  const live = make("live");
  const preview = make("preview");
  assert.ok(file.vectors.length >= 9);
  const names = file.vectors.map((v) => v.name);
  for (const n of ["live", "preview", "another app", "expired", "exp - iat = 61", "extra claim"]) assert.ok(names.includes(n), n);
  for (const v of file.vectors) {
    const tok = v.parts.join(".");
    // The payload is exactly the claims of the file.
    assert.deepEqual(JSON.parse(Buffer.from(v.parts[1] as string, "base64url").toString("utf8")), v.claims, v.name);
    assert.deepEqual(JSON.parse(Buffer.from(v.parts[0] as string, "base64url").toString("utf8")), { typ: "JWT", alg: "EdDSA", kid: file.kid });
    for (const [verifier, expected] of [
      [live, v.live],
      [preview, v.preview],
    ] as const) {
      if (expected === "accept") {
        const u = await verifier.verify(tok);
        assert.deepEqual(u.roles, v.claims["roles"], v.name);
      } else {
        await assert.rejects(verifier.verify(tok), (e) => (e as RuumTokenError).reason === (expected as RefusalReason), `${v.name}: ${expected}`);
      }
    }
  }
});
