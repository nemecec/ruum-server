import { test } from "node:test";
import assert from "node:assert/strict";
import { createHmac, generateKeyPairSync, sign } from "node:crypto";
import { jwksOf, publicJwk } from "../src/dev.js";
import {
  CLOCK_SKEW_SECONDS,
  RuumConfigError,
  RuumTokenError,
  UserTokenVerifier,
  configFromEnv,
  type RefusalReason,
} from "../src/verify.js";
import {
  APP_B,
  ISSUER,
  LIVE_A,
  NOW,
  PREVIEW_A,
  b64,
  claims,
  key,
  kid,
  otherKey,
  previewVerifier,
  rawToken,
  token,
  verifier,
  without,
} from "./helpers.js";

/** Asserts that `t` is refused with `reason`, and that the error holds no part of the token. */
async function refused(v: UserTokenVerifier, t: unknown, reason: RefusalReason): Promise<void> {
  await assert.rejects(v.verify(t), (e: unknown) => {
    assert.ok(e instanceof RuumTokenError, "one error type");
    assert.equal(e.reason, reason);
    if (typeof t === "string" && t.length > 0) {
      const text = `${e.message} ${e.stack ?? ""} ${JSON.stringify(e)}`;
      for (const part of t.split(".")) {
        if (part.length >= 8) assert.ok(!text.includes(part), "the error never holds the token");
      }
    }
    return true;
  });
}

async function accepted(v: UserTokenVerifier, t: string): Promise<void> {
  const user = await v.verify(t);
  assert.ok(user.sub.length > 0);
}

// ----------------------------------------------------------------- the STEP3 done-when

test("iss, aud, typ, env and exp are checked against RUUM_ISSUER, RUUM_AUDIENCE and RUUM_ENV", async () => {
  const v = verifier();
  const cases: [Record<string, unknown>, RefusalReason][] = [
    [{ iss: "https://evil.example" }, "wrong_issuer"],
    [{ iss: `${ISSUER}/` }, "wrong_issuer"],
    [{ aud: `app:${APP_B}` }, "wrong_audience"],
    [{ typ: "app_auth_state" }, "wrong_type"],
    [{ typ: "app_delegated" }, "wrong_type"],
    [{ env: "preview" }, "wrong_env"],
    [{ iat: NOW - 70, exp: NOW - 10 }, "expired"],
  ];
  for (const [over, reason] of cases) {
    await refused(v, token(claims(over)), reason);
  }
  // Positive control: the same claims with the one field fixed.
  await accepted(v, token(claims()));
});

test("the user object carries the claims", async () => {
  const user = await verifier().verify(token(claims({ name: "Mari", email: "m@example.com", locale: "et" })));
  assert.equal(user.sub, "pairwise:visitor1");
  assert.equal(user.tenant, "acme");
  assert.deepEqual(user.roles, ["member"]);
  assert.deepEqual(user.node_roles, { db: "edit" });
  assert.equal(user.actor, "user");
  assert.equal(user.actorKnown, true);
  assert.equal(user.anonymous, false);
  assert.equal(user.name, "Mari");
  assert.equal(user.email, "m@example.com");
  assert.equal(user.locale, "et");
  assert.equal(user.claims["jti"], "j-1");
  assert.ok(Object.isFrozen(user.claims));
});

// ----------------------------------------------------------------- audiences and envs

test("another app's token is refused by app A", async () => {
  await refused(verifier(), token(claims({ aud: `app:${APP_B}` })), "wrong_audience");
  await accepted(verifier(), token(claims({ aud: LIVE_A })));
});

test("a preview token is refused by the live app, and a live token by the preview", async () => {
  const previewToken = token(claims({ aud: PREVIEW_A, env: "preview" }));
  const liveToken = token(claims());
  await refused(verifier(), previewToken, "wrong_audience");
  await refused(previewVerifier(), liveToken, "wrong_audience");
  // Controls: each token at its own app.
  await accepted(previewVerifier(), previewToken);
  await accepted(verifier(), liveToken);
});

test("aud is compared exactly, never as a prefix", async () => {
  // The live audience is a prefix of the preview audience, and the other way round fails too.
  await refused(verifier(), token(claims({ aud: `${LIVE_A}/preview/x` })), "wrong_audience");
  await refused(previewVerifier(), token(claims({ aud: LIVE_A, env: "preview" })), "wrong_audience");
  await refused(verifier(), token(claims({ aud: [LIVE_A] })), "wrong_audience");
  await refused(verifier(), token(claims({ aud: `${LIVE_A} ` })), "wrong_audience");
  await refused(verifier(), token(claims({ aud: LIVE_A.toUpperCase() })), "wrong_audience");
  await accepted(verifier(), token(claims({ aud: LIVE_A })));
});

test("the right aud with the wrong env is refused", async () => {
  await refused(verifier(), token(claims({ env: "preview" })), "wrong_env");
  await refused(previewVerifier(), token(claims({ aud: PREVIEW_A, env: "live" })), "wrong_env");
  await accepted(verifier(), token(claims({ env: "live" })));
  await accepted(previewVerifier(), token(claims({ aud: PREVIEW_A, env: "preview" })));
});

// ----------------------------------------------------------------- header and signature

test("alg none, HS256 with the public key as the secret, RS256 and a missing alg are refused", async () => {
  const v = verifier();
  const payload = JSON.stringify(claims());
  const none = `${b64(JSON.stringify({ alg: "none", typ: "JWT", kid }))}.${b64(payload)}.`;
  await refused(v, none, "malformed");
  const noneWithSig = `${b64(JSON.stringify({ alg: "none", typ: "JWT", kid }))}.${b64(payload)}.${b64("x".repeat(64))}`;
  await refused(v, noneWithSig, "alg_not_allowed");

  const x = publicJwk(key)["x"] as string;
  for (const secret of [Buffer.from(x, "base64url"), Buffer.from(x), Buffer.from(JSON.stringify(publicJwk(key)))]) {
    const input = `${b64(JSON.stringify({ alg: "HS256", typ: "JWT", kid }))}.${b64(payload)}`;
    const mac = createHmac("sha256", secret).update(input).digest("base64url");
    await refused(v, `${input}.${mac}`, "alg_not_allowed");
  }

  const rsa = generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey;
  const rsInput = `${b64(JSON.stringify({ alg: "RS256", typ: "JWT", kid }))}.${b64(payload)}`;
  const rs = sign("sha256", Buffer.from(rsInput), rsa).toString("base64url");
  await refused(v, `${rsInput}.${rs}`, "alg_not_allowed");

  await refused(v, token(claims(), key, { typ: "JWT", kid }), "alg_not_allowed");
  await refused(v, token(claims(), key, { alg: "eddsa", typ: "JWT", kid }), "alg_not_allowed");
  await refused(v, token(claims(), key, { alg: "Ed25519", typ: "JWT", kid }), "alg_not_allowed");
  // Control: the same claims with alg EdDSA.
  await accepted(v, token(claims(), key, { alg: "EdDSA", typ: "JWT", kid }));
});

test("a header with jku, x5u or jwk is refused and never followed", async () => {
  let fetches = 0;
  const v = verifier({
    jwks: undefined,
    fetchText: async () => {
      fetches++;
      return JSON.stringify(jwksOf(key));
    },
  });
  const jwk = publicJwk(otherKey);
  for (const extra of [
    { jku: "https://evil.example/jwks.json" },
    { x5u: "https://evil.example/cert.pem" },
    { jwk },
    { x5c: ["MIIB"] },
    { crit: ["exp"] },
  ]) {
    const signer = "jwk" in extra ? otherKey : key;
    await refused(v, token(claims(), signer, { alg: "EdDSA", typ: "JWT", kid, ...extra }), "bad_header");
  }
  assert.equal(fetches, 0, "nothing was fetched for a refused header");
  // Control: the plain header is accepted (and loads the issuer's key set once).
  await accepted(v, token(claims()));
  assert.equal(fetches, 1);
});

test("a token signed by a key not in the set is refused", async () => {
  const v = verifier();
  // The kid of the real key, but signed by another key.
  await refused(v, token(claims(), otherKey, { typ: "JWT", alg: "EdDSA", kid }), "bad_signature");
  // The other key's own kid: not in the set.
  await refused(v, token(claims(), otherKey), "unknown_kid");
  // A missing or odd kid.
  await refused(v, token(claims(), key, { typ: "JWT", alg: "EdDSA" }), "bad_header");
  await refused(v, token(claims(), key, { typ: "JWT", alg: "EdDSA", kid: 7 }), "bad_header");
  await accepted(v, token(claims(), key));
});

test("a valid signature over different claims (claims swapped) is refused", async () => {
  const v = verifier();
  const a = token(claims({ roles: ["member"] })).split(".");
  const b = token(claims({ roles: ["admin"] })).split(".");
  await refused(v, `${a[0]}.${b[1]}.${a[2]}`, "bad_signature");
  await refused(v, `${b[0]}.${a[1]}.${b[2]}`, "bad_signature");
  // A changed header with the old signature.
  const h2 = b64(JSON.stringify({ alg: "EdDSA", kid, typ: "JWT" }));
  await refused(v, `${h2}.${a[1]}.${a[2]}`, "bad_signature");
  await accepted(v, a.join("."));
  await accepted(v, b.join("."));
});

test("a duplicate JSON key is refused (the SDK refuses, it never picks the first or last)", async () => {
  const v = verifier();
  const header = JSON.stringify({ typ: "JWT", alg: "EdDSA", kid });
  const good = JSON.stringify(claims());
  // "aud" twice: the first is app B, the last is app A (JSON.parse would keep app A).
  const dup = good.replace('"aud":', `"aud":"app:${APP_B}","aud":`);
  assert.equal(JSON.parse(dup).aud, LIVE_A, "JSON.parse alone would accept it");
  await refused(v, rawToken(header, dup), "duplicate_claim");
  // An escaped spelling of the same key is the same key.
  const dupEscaped = good.replace('"aud":', `"a\\u0075d":"app:${APP_B}","aud":`);
  await refused(v, rawToken(header, dupEscaped), "duplicate_claim");
  // Nested duplicates too.
  const dupNested = good.replace('"node_roles":{"db":"edit"}', '"node_roles":{"db":"view","db":"edit"}');
  await refused(v, rawToken(header, dupNested), "duplicate_claim");
  // In the header.
  await refused(v, rawToken(`{"alg":"none","alg":"EdDSA","typ":"JWT","kid":"${kid}"}`, good), "bad_header");
  // Control: the same payload without the duplicate.
  await accepted(v, rawToken(header, good));
});

test("malformed tokens are refused before any key is used", async () => {
  const v = verifier();
  const good = token(claims());
  const [h, p, s] = good.split(".") as [string, string, string];
  await refused(v, undefined, "missing");
  await refused(v, "", "missing");
  await refused(v, 42, "malformed");
  await refused(v, `${h}.${p}`, "malformed");
  await refused(v, `${good}.x`, "malformed");
  await refused(v, `${h}=.${p}.${s}`, "malformed");
  await refused(v, `${h}.${p}.${s}=`, "malformed");
  await refused(v, `${h}.+${p}.${s}`, "malformed");
  await refused(v, `${h}.${p}/.${s}`, "malformed");
  await refused(v, `${good}, ${good}`, "malformed");
  await refused(v, `Bearer ${good}`, "malformed");
  await refused(v, `${h}.${p}.${s}${"A".repeat(9000)}`, "too_large");
  // A non-canonical last character of the signature (unused bits set).
  const last = s.at(-1) as string;
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  const idx = alphabet.indexOf(last);
  const twin = alphabet[(idx & ~0xf) | ((idx + 1) & 0xf)] as string;
  if (twin !== last) await refused(v, `${h}.${p}.${s.slice(0, -1)}${twin}`, "malformed");
  await refused(v, `${b64("[1]")}.${p}.${s}`, "bad_header");
  await accepted(v, good);
});

// ----------------------------------------------------------------- times (0006 G point 2)

test("exp in the past, exp - iat over 60, missing or string iat/exp and iat in the future are refused", async () => {
  const v = verifier();
  await refused(v, token(claims({ iat: NOW - 61, exp: NOW })), "expired");
  await accepted(v, token(claims({ iat: NOW - 59, exp: NOW + 1 })));

  await refused(v, token(claims({ iat: NOW - 1, exp: NOW + 60 })), "lifetime_too_long");
  await accepted(v, token(claims({ iat: NOW, exp: NOW + 60 })));
  await refused(v, token(claims({ iat: NOW + 1, exp: NOW + 1 })), "lifetime_too_long");

  await refused(v, token(without(claims(), "iat")), "bad_time");
  await refused(v, token(without(claims(), "exp")), "bad_time");
  await refused(v, token(claims({ iat: String(NOW - 5) })), "bad_time");
  await refused(v, token(claims({ exp: String(NOW + 55) })), "bad_time");
  await refused(v, token(claims({ iat: NOW - 5.5 })), "bad_time");
  await refused(v, token(claims({ exp: null })), "bad_time");
  await refused(v, token(claims({ exp: 1e300 })), "bad_time");
  await accepted(v, token(claims({ iat: NOW - 5, exp: NOW + 55 })));

  const skew = CLOCK_SKEW_SECONDS;
  assert.equal(skew, 5);
  await refused(v, token(claims({ iat: NOW + skew + 1, exp: NOW + skew + 30 })), "issued_in_future");
  await accepted(v, token(claims({ iat: NOW + skew, exp: NOW + skew + 30 })));
});

// ----------------------------------------------------------------- the open contract

test("an extra claim and an unknown actor and sub prefix are accepted and do not change the roles", async () => {
  const v = verifier();
  const base = await v.verify(token(claims()));
  const extra = await v.verify(token(claims({ x_later: { a: 1 }, admin_roles: ["owner"], scope: "all" })));
  assert.deepEqual(extra.roles, base.roles);
  assert.equal(extra.claims["x_later"] !== undefined, true);
  const odd = await v.verify(token(claims({ actor: "robot", sub: "remote:abc-1" })));
  assert.equal(odd.actor, "robot");
  assert.equal(odd.actorKnown, false);
  assert.equal(odd.sub, "remote:abc-1");
  assert.deepEqual(odd.roles, base.roles);
  // A claim named like a header member is just a claim.
  await accepted(v, token(claims({ jku: "https://evil.example", kid: "x" })));
});

test("claims the user object needs must have their types", async () => {
  const v = verifier();
  await refused(v, token(claims({ roles: "member" })), "bad_claims");
  await refused(v, token(claims({ roles: [1] })), "bad_claims");
  await refused(v, token(without(claims(), "sub")), "bad_claims");
  await refused(v, token(claims({ sub: "no-prefix" })), "bad_claims");
  await refused(v, token(claims({ node_roles: [] })), "bad_claims");
  await refused(v, token(claims({ actor: 3 })), "bad_claims");
  await refused(v, token(claims({ email: 3 })), "bad_claims");
  await refused(v, token(without(claims(), "tenant")), "bad_claims");
  await accepted(v, token(claims()));
});

test("errors never hold the token or claim values", async () => {
  const v = verifier();
  const secretish = "pairwise:VeryPrivateValue42";
  const t = token(claims({ sub: secretish, aud: `app:${APP_B}` }));
  await assert.rejects(v.verify(t), (e: unknown) => {
    const text = `${(e as Error).message}\n${(e as Error).stack}`;
    assert.ok(!text.includes(t));
    assert.ok(!text.includes("VeryPrivateValue42"));
    assert.ok(!text.includes(APP_B));
    return true;
  });
});

// ----------------------------------------------------------------- configuration

test("a missing RUUM_ISSUER, RUUM_AUDIENCE or RUUM_ENV is a start-up error", () => {
  const env = { RUUM_ISSUER: ISSUER, RUUM_AUDIENCE: LIVE_A, RUUM_ENV: "live" };
  for (const k of Object.keys(env)) {
    const e = { ...env, [k]: "" };
    assert.throws(() => configFromEnv(e), RuumConfigError);
    const m: Record<string, string> = { ...env };
    delete m[k];
    assert.throws(() => configFromEnv(m), RuumConfigError);
  }
  assert.deepEqual(configFromEnv(env), { issuer: ISSUER, audience: LIVE_A, env: "live", jwksFile: undefined });
  assert.throws(() => new UserTokenVerifier({ issuer: ISSUER, audience: "", env: "live" }), RuumConfigError);
  assert.throws(() => new UserTokenVerifier({ issuer: ISSUER, audience: LIVE_A, env: "staging" }), RuumConfigError);
  assert.throws(() => new UserTokenVerifier({ issuer: ISSUER, audience: PREVIEW_A, env: "live" }), RuumConfigError);
  assert.throws(() => new UserTokenVerifier({ issuer: "http://ruum.example", audience: LIVE_A, env: "live" }), RuumConfigError);
  assert.throws(() => new UserTokenVerifier({ issuer: ISSUER, audience: LIVE_A, env: "live", jwksFile: "/nonexistent/jwks.json" }), RuumConfigError);
  // https issuer without a file: the key set URL; nothing is fetched at start.
  assert.ok(new UserTokenVerifier({ issuer: ISSUER, audience: LIVE_A, env: "live" }));
});
