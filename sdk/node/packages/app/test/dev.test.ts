import { test } from "node:test";
import { generateKeyPairSync } from "node:crypto";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, existsSync, symlinkSync, writeFileSync, lstatSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  DevModeError,
  assertDevAllowed,
  devClaims,
  devJwksOf,
  initDev,
  jwksOf,
  loadDevKey,
  mintDevToken,
  setNoFollowForTests,
  privateKeyFromSeed,
  publicJwk,
  signToken,
} from "../src/dev.js";
import { main } from "../src/cli.js";
import { RuumConfigError, RuumTokenError, UserTokenVerifier, verifyUserToken, type RefusalReason, type VerifierConfig } from "../src/verify.js";
import { DEV_KEY_MARK } from "../src/keys.js";
import { ISSUER, LIVE_A, NOW, PREVIEW_A, claims, key } from "./helpers.js";

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

test("dev mode refuses padded or upper-case RUUM_ENV=live and NODE_ENV=production", () => {
  for (const env of [{ RUUM_ENV: " live " }, { RUUM_ENV: "live\n" }, { RUUM_ENV: "LIVE" }, { NODE_ENV: " production " }, { NODE_ENV: "Production" }]) {
    assert.throws(() => assertDevAllowed(env), DevModeError, JSON.stringify(env));
    const t = tmp();
    try {
      assert.throws(() => initDev(t.dir, env), DevModeError);
      assert.equal(existsSync(t.dir), false, "nothing was written");
      initDev(t.dir, DEV_ENV);
      assert.throws(() => mintDevToken({ audience: LIVE_A }, t.dir, env), DevModeError);
    } finally {
      t.done();
    }
  }
  // Control: padded dev values are allowed.
  assert.doesNotThrow(() => assertDevAllowed({ RUUM_ENV: " preview ", NODE_ENV: " development " }));
});

test("a verifier with a real issuer refuses to start with a dev-marked key set", async () => {
  const devKey = generateKeyPairSync("ed25519").privateKey;
  const marked = devJwksOf(devKey);
  assert.equal(marked.keys[0]?.[DEV_KEY_MARK], true);
  // A dev token that claims the real issuer, signed by the dev key.
  const forged = signToken(claims(), devKey);
  for (const over of [{ jwks: marked }, { jwks: { keys: [...jwksOf(key).keys, ...marked.keys] } }]) {
    assert.throws(() => new UserTokenVerifier({ issuer: ISSUER, audience: LIVE_A, env: "live", now: () => NOW, ...over }), RuumConfigError);
  }
  // Control: the same key set with a dev issuer verifies a dev token.
  const local = new UserTokenVerifier({ issuer: "http://localhost:8080", audience: LIVE_A, env: "live", jwks: marked, now: () => NOW });
  assert.equal((await local.verify(signToken(claims({ iss: "http://localhost:8080" }), devKey))).sub, "pairwise:visitor1");
  // Control: an unmarked key set with the real issuer verifies; the forged token is refused there.
  const real = new UserTokenVerifier({ issuer: ISSUER, audience: LIVE_A, env: "live", jwks: jwksOf(key), now: () => NOW });
  assert.ok(await real.verify(signToken(claims(), key)));
  await assert.rejects(real.verify(forged), (e) => (e as RuumTokenError).reason === "unknown_kid");
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
    assert.equal(jwks.keys[0][DEV_KEY_MARK], true, "the JWKS marks the dev key");
    // A second init keeps the key.
    initDev(t.dir, DEV_ENV);
    assert.deepEqual(JSON.parse(readFileSync(f.jwksFile, "utf8")), jwks);

    const tok = mintDevToken({ audience: PREVIEW_A, roles: ["member"] }, t.dir, DEV_ENV);
    const local = new UserTokenVerifier({ issuer: "http://localhost", audience: PREVIEW_A, env: "preview", jwksFile: f.jwksFile });
    const user = await local.verify(tok);
    assert.deepEqual(user.roles, ["member"]);
    // A real deployment (https issuer) pointed at the dev key file does not start at all.
    assert.throws(
      () => new UserTokenVerifier({ issuer: "https://ruum.space", audience: PREVIEW_A, env: "preview", jwksFile: f.jwksFile }),
      RuumConfigError,
    );
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

test("a change to the config after start-up cannot move the issuer, audience or env past the start-up checks", async () => {
  // Kimi review of O-5 (run 13), finding 3: the verifier allowed dev keys for the local issuer,
  // then read the issuer from the caller's (changed) object, so a dev-signed token with the real
  // issuer passed. The verifier now keeps copies of the checked values.
  const devKey = generateKeyPairSync("ed25519").privateKey;
  const local = "http://localhost";
  const otherApp = "app:0190b6c4-a00d-7cc1-9a4b-00000000000b";
  const make = (): VerifierConfig => ({ issuer: local, audience: LIVE_A, env: "live", jwks: devJwksOf(devKey), now: () => NOW });
  const forged = signToken(claims({ iss: ISSUER }), devKey);
  const localToken = signToken(claims({ iss: local }), devKey);
  const otherAppToken = signToken(claims({ iss: local, aud: otherApp }), devKey);
  const previewToken = signToken(claims({ iss: local, aud: PREVIEW_A, env: "preview" }), devKey);
  const refused = (reason: RefusalReason) => (e: unknown) => e instanceof RuumTokenError && e.reason === reason;

  // Direct construction.
  const cfg = make();
  const v = new UserTokenVerifier(cfg);
  assert.equal((await v.verify(localToken)).sub, "pairwise:visitor1", "control: the local dev token passes");
  cfg.issuer = ISSUER;
  cfg.audience = otherApp;
  await assert.rejects(v.verify(forged), refused("wrong_issuer"));
  await assert.rejects(v.verify(otherAppToken), refused("wrong_audience"));
  cfg.audience = PREVIEW_A;
  cfg.env = "preview";
  await assert.rejects(v.verify(previewToken), refused("wrong_audience"));
  assert.equal((await v.verify(localToken)).sub, "pairwise:visitor1", "the verifier still checks the first values");

  // verifyUserToken keeps one verifier per config object: the same holds for it.
  const cfg2 = make();
  assert.equal((await verifyUserToken(localToken, cfg2)).sub, "pairwise:visitor1");
  cfg2.issuer = ISSUER;
  cfg2.audience = otherApp;
  await assert.rejects(verifyUserToken(forged, cfg2), refused("wrong_issuer"));
  await assert.rejects(verifyUserToken(otherAppToken, cfg2), refused("wrong_audience"));
  assert.equal((await verifyUserToken(localToken, cfg2)).sub, "pairwise:visitor1");
});

test("dev mode never follows a symbolic link at the dev folder or its files", () => {
  // Kimi review of O-5 (run 13), finding 2: a checkout could plant symbolic links so that
  // initDev puts the private key, or writes over a file, outside the dev folder.
  const base = mkdtempSync(join(tmpdir(), "ruum-o5-link-"));
  try {
    const outside = join(base, "outside");
    mkdirSync(outside);
    const sentinel = join(outside, "sentinel.txt");
    const keep = "do not touch\n";
    const fresh = (name: string): string => {
      const d = join(base, name, ".ruum-dev");
      mkdirSync(d, { recursive: true });
      return d;
    };
    const unchanged = () => assert.equal(readFileSync(sentinel, "utf8"), keep, "the outside file is unchanged");

    // 1. A dangling key link: the key would be made at the target, outside the folder.
    writeFileSync(sentinel, keep);
    const d1 = fresh("dangling");
    const target = join(outside, "key.pem");
    symlinkSync(target, join(d1, "key.pem"));
    assert.throws(() => initDev(d1, DEV_ENV), DevModeError);
    assert.equal(existsSync(target), false, "no key was made outside the dev folder");

    // 2. A key link to an existing file: neither read as the key nor chmod-ed.
    const d2 = fresh("keylink");
    symlinkSync(sentinel, join(d2, "key.pem"));
    const modeBefore = statSync(sentinel).mode;
    assert.throws(() => initDev(d2, DEV_ENV), DevModeError);
    assert.throws(() => loadDevKey(d2), DevModeError);
    assert.equal(statSync(sentinel).mode, modeBefore, "the outside file keeps its mode");
    unchanged();

    // 3. A .gitignore link: the outside file is not overwritten with "*".
    const d3 = fresh("gitignore");
    symlinkSync(sentinel, join(d3, ".gitignore"));
    assert.throws(() => initDev(d3, DEV_ENV), DevModeError);
    unchanged();

    // 4. A jwks.json link: the outside file is not overwritten with the key set.
    const d4 = fresh("jwks");
    symlinkSync(sentinel, join(d4, "jwks.json"));
    assert.throws(() => initDev(d4, DEV_ENV), DevModeError);
    unchanged();

    // 5. The dev folder itself is a link: nothing is written through it.
    const d5parent = join(base, "dirlink");
    mkdirSync(d5parent);
    const d5 = join(d5parent, ".ruum-dev");
    symlinkSync(outside, d5);
    assert.throws(() => initDev(d5, DEV_ENV), DevModeError);
    assert.throws(() => mintDevToken({ audience: LIVE_A }, d5, DEV_ENV), DevModeError);
    assert.equal(existsSync(join(outside, ".gitignore")), false);
    assert.equal(existsSync(join(outside, "key.pem")), false);
    assert.equal(existsSync(join(outside, "jwks.json")), false);
    unchanged();

    // Control: a plain dev folder works, twice (the second run keeps the key), and its files are plain.
    const d6 = join(base, "plain", ".ruum-dev");
    const f = initDev(d6, DEV_ENV);
    const first = readFileSync(f.keyFile, "utf8");
    initDev(d6, DEV_ENV);
    assert.equal(readFileSync(f.keyFile, "utf8"), first, "the key is kept");
    assert.equal(statSync(f.keyFile).mode & 0o777, 0o600);
    for (const p of [f.keyFile, f.jwksFile, join(f.dir, ".gitignore")]) assert.ok(lstatSync(p).isFile(), p);
    assert.ok(mintDevToken({ audience: LIVE_A }, d6, DEV_ENV).split(".").length === 3);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test("without O_NOFOLLOW (Windows) a key link is still refused, also by loadDevKey and mintDevToken", () => {
  // Kimi re-check of O-5 (run 13), finding 1: where O_NOFOLLOW is missing, loadDevKey opened the
  // key without an lstat, so it followed .ruum-dev/key.pem to a valid key outside and signed with it.
  const before = setNoFollowForTests(0);
  const base = mkdtempSync(join(tmpdir(), "ruum-o5-nofollow-"));
  try {
    const outside = join(base, "outside");
    mkdirSync(outside);
    const outsideKey = join(outside, "other.pem");
    const pem = generateKeyPairSync("ed25519").privateKey.export({ format: "pem", type: "pkcs8" }) as string;
    writeFileSync(outsideKey, pem, { mode: 0o644 });
    const d = join(base, "app", ".ruum-dev");
    mkdirSync(d, { recursive: true });
    symlinkSync(outsideKey, join(d, "key.pem"));
    assert.throws(() => loadDevKey(d), DevModeError);
    assert.throws(() => mintDevToken({ audience: LIVE_A }, d, DEV_ENV), DevModeError);
    assert.throws(() => initDev(d, DEV_ENV), DevModeError);
    assert.equal(statSync(outsideKey).mode & 0o777, 0o644, "the outside key keeps its mode");
    assert.equal(readFileSync(outsideKey, "utf8"), pem);
    // .gitignore and jwks.json links are refused the same way.
    const g = join(base, "app2", ".ruum-dev");
    mkdirSync(g, { recursive: true });
    const sentinel = join(outside, "sentinel.txt");
    writeFileSync(sentinel, "keep\n");
    symlinkSync(sentinel, join(g, ".gitignore"));
    assert.throws(() => initDev(g, DEV_ENV), DevModeError);
    assert.equal(readFileSync(sentinel, "utf8"), "keep\n");
    // Control: a plain dev folder still works on this path.
    const ok = join(base, "plain", ".ruum-dev");
    initDev(ok, DEV_ENV);
    assert.equal(mintDevToken({ audience: LIVE_A }, ok, DEV_ENV).split(".").length, 3);
  } finally {
    setNoFollowForTests(before);
    rmSync(base, { recursive: true, force: true });
  }
});
