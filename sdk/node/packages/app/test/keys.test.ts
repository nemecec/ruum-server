import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { generateKeyPairSync } from "node:crypto";
import { KeySet, keysFromJwks, MAX_KEYS } from "../src/keys.js";
import { httpsGet } from "../src/proxy.js";
import { jwksOf, publicJwk } from "../src/dev.js";
import { claims, key, otherKey, token, verifier, ISSUER, LIVE_A, NOW } from "./helpers.js";
import { RuumTokenError, UserTokenVerifier } from "../src/verify.js";

test("a flood of unknown kids refreshes the key set at most once per interval", async () => {
  let fetches = 0;
  let clock = 1_000_000;
  const ks = new KeySet({
    url: "https://ruum.example/.well-known/app-jwks.json",
    minRefreshMs: 30_000,
    nowMs: () => clock,
    fetchText: async () => {
      fetches++;
      return JSON.stringify(jwksOf(key));
    },
  });
  const kid = publicJwk(key)["kid"] as string;
  assert.ok(await ks.get(kid));
  assert.equal(fetches, 1);
  await Promise.all(Array.from({ length: 200 }, (_, i) => ks.get(`made-up-${i}`)));
  assert.equal(fetches, 1, "no refresh inside the interval");
  clock += 30_001;
  await Promise.all(Array.from({ length: 200 }, (_, i) => ks.get(`made-up-again-${i}`)));
  assert.equal(fetches, 2, "one refresh after the interval, shared by concurrent lookups");
  // A known kid never fetches.
  clock += 60_000;
  assert.ok(await ks.get(kid));
  assert.equal(fetches, 2);
});

test("a rotated key in RUUM_APP_JWKS_FILE is picked up on an unknown kid", async () => {
  const dir = mkdtempSync(join(tmpdir(), "ruum-o5-jwks-"));
  try {
    const file = join(dir, "jwks.json");
    writeFileSync(file, JSON.stringify(jwksOf(key)));
    const v = new UserTokenVerifier({ issuer: ISSUER, audience: LIVE_A, env: "live", jwksFile: file, minRefreshMs: 0, now: () => NOW });
    assert.ok(await v.verify(token(claims())));
    await assert.rejects(v.verify(token(claims(), otherKey)), (e) => (e as RuumTokenError).reason === "unknown_kid");
    writeFileSync(file, JSON.stringify({ keys: [...jwksOf(key).keys, ...jwksOf(otherKey).keys] }));
    assert.ok(await v.verify(token(claims(), otherKey)));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("only usable public Ed25519 keys are kept, at most MAX_KEYS", () => {
  const good = publicJwk(key);
  const keys: Record<string, unknown>[] = [
    { ...good, kid: "rsa", kty: "RSA" },
    { ...good, kid: "x25519", crv: "X25519" },
    { ...good, kid: "es", alg: "ES256" },
    { ...good, kid: "enc", use: "enc" },
    { ...good, kid: "" },
    { ...good, kid: "priv", d: "AAAA" },
    { ...good, kid: "badx", x: "short" },
    good,
  ];
  const m = keysFromJwks({ keys });
  assert.deepEqual([...m.keys()], [good["kid"]]);
  const many = Array.from({ length: 40 }, (_, i) => ({ ...publicJwk(generateKeyPairSync("ed25519").privateKey), kid: `k${i}` }));
  assert.equal(keysFromJwks({ keys: many }).size, MAX_KEYS);
  assert.equal(keysFromJwks(null).size, 0);
  assert.equal(keysFromJwks({ keys: "x" }).size, 0);
});

test("a key set fetch that fails keeps the verifier refusing, never accepting", async () => {
  const v = verifier({
    jwks: undefined,
    fetchText: async () => {
      throw new Error("down");
    },
  });
  await assert.rejects(v.verify(token(claims())), (e) => (e as RuumTokenError).reason === "unknown_kid");
});

test("the key set GET goes through HTTPS_PROXY with CONNECT to the issuer's host", async () => {
  const seen: string[] = [];
  const proxy = http.createServer((_req, res) => {
    res.statusCode = 405;
    res.end();
  });
  proxy.on("connect", (req, socket) => {
    seen.push(`${req.method} ${req.url}`);
    socket.end("HTTP/1.1 403 Forbidden\r\nX-Ruum-Egress-Deny: test\r\n\r\n");
  });
  await new Promise<void>((r) => proxy.listen(0, "127.0.0.1", r));
  const port = (proxy.address() as AddressInfo).port;
  try {
    await assert.rejects(
      httpsGet("https://jwks.ruum.example/.well-known/app-jwks.json", { maxBytes: 1000, timeoutMs: 3000, proxy: `http://127.0.0.1:${port}` }),
      /refused the tunnel \(status 403\)/,
    );
    assert.deepEqual(seen, ["CONNECT jwks.ruum.example:443"]);
    await assert.rejects(httpsGet("http://jwks.ruum.example/x", { maxBytes: 1, timeoutMs: 100 }), /only https/);
    await assert.rejects(httpsGet("https://u:p@jwks.ruum.example/x", { maxBytes: 1, timeoutMs: 100 }), /credentials/);
    await assert.rejects(httpsGet("https://jwks.ruum.example/x", { maxBytes: 1, timeoutMs: 100, proxy: "socks5://127.0.0.1:1" }), /http:\/\//);
  } finally {
    proxy.close();
  }
});
