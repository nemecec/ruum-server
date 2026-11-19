import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { ruumUser, withRuumUser, type RequestWithUser } from "../src/middleware.js";
import { RuumConfigError } from "../src/verify.js";
import { claims, token, verifier } from "./helpers.js";

async function serve(handler: http.RequestListener): Promise<{ url: string; close: () => void }> {
  const server = http.createServer(handler);
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
  const port = (server.address() as AddressInfo).port;
  return { url: `http://127.0.0.1:${port}/`, close: () => server.close() };
}

/** A GET with raw header lines (so a header can be sent twice). */
function get(url: string, headers: [string, string][] = []): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = http.request(url, { method: "GET" }, (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () => resolve({ status: res.statusCode ?? 0, body }));
    });
    for (const [k, v] of headers) {
      const prev = req.getHeader(k);
      req.setHeader(k, prev === undefined ? v : ([] as string[]).concat(prev as string, v));
    }
    req.on("error", reject);
    req.end();
  });
}

test("node:http helper: 401 without Ruum-User, the user with a valid one", async () => {
  const s = await serve(withRuumUser((_req, res, user) => res.end(`hello ${user.sub} ${user.roles.join(",")}`), { verifier: verifier() }));
  try {
    const none = await get(s.url);
    assert.equal(none.status, 401);
    assert.equal(JSON.parse(none.body).error.code, "unauthenticated");
    const t = token(claims());
    const ok = await get(s.url, [["Ruum-User", t]]);
    assert.equal(ok.status, 200);
    assert.equal(ok.body, "hello pairwise:visitor1 member");
    // A bad token: 401, and the answer never echoes it.
    const bad = token(claims({ env: "preview" }));
    const r = await get(s.url, [["Ruum-User", bad]]);
    assert.equal(r.status, 401);
    assert.ok(!r.body.includes(bad));
    assert.ok(r.body.includes("wrong_env"));
    // Two Ruum-User headers: refused.
    const two = await get(s.url, [["Ruum-User", t], ["Ruum-User", t]]);
    assert.equal(two.status, 401);
  } finally {
    s.close();
  }
});

test("no other header is trusted for identity", async () => {
  const s = await serve(withRuumUser((_req, res, user) => res.end(user.sub), { verifier: verifier() }));
  try {
    for (const h of ["X-Forwarded-User", "X-User", "Remote-User", "Authorization", "Ruum_User", "X-Ruum-User"]) {
      const r = await get(s.url, [[h, token(claims())]]);
      assert.equal(r.status, 401, `${h} must not sign anyone in`);
    }
    const ok = await get(s.url, [["X-Forwarded-User", "pairwise:attacker"], ["Ruum-User", token(claims())]]);
    assert.equal(ok.body, "pairwise:visitor1");
  } finally {
    s.close();
  }
});

test("anonymous visitors only for a public app, and the header is still required", async () => {
  const anon = token(claims({ sub: "anonymous:visit1", actor: "anonymous", roles: [] }));
  const closed = await serve(withRuumUser((_q, res) => res.end("in"), { verifier: verifier() }));
  const open = await serve(withRuumUser((_q, res, u) => res.end(`in ${u.anonymous}`), { verifier: verifier(), allowAnonymous: true }));
  try {
    assert.equal((await get(closed.url, [["Ruum-User", anon]])).status, 401);
    const r = await get(open.url, [["Ruum-User", anon]]);
    assert.equal(r.status, 200);
    assert.equal(r.body, "in true");
    assert.equal((await get(open.url)).status, 401, "no header: 401 even for a public app");
    // Control: a signed-in user is let in at the closed app.
    assert.equal((await get(closed.url, [["Ruum-User", token(claims())]])).status, 200);
  } finally {
    closed.close();
    open.close();
  }
});

test("Express-style middleware puts the user on the request", async () => {
  const mw = ruumUser({ verifier: verifier() });
  const s = await serve((req, res) => {
    mw(req, res, (err?: unknown) => {
      if (err) {
        res.statusCode = 500;
        res.end();
        return;
      }
      res.end((req as RequestWithUser).ruumUser?.tenant ?? "none");
    });
  });
  try {
    assert.equal((await get(s.url)).status, 401);
    const r = await get(s.url, [["ruum-user", token(claims())]]);
    assert.equal(r.status, 200);
    assert.equal(r.body, "acme");
  } finally {
    s.close();
  }
});

test("making the middleware without the environment is a start-up error", () => {
  const saved = { ...process.env };
  try {
    delete process.env["RUUM_ISSUER"];
    delete process.env["RUUM_AUDIENCE"];
    delete process.env["RUUM_ENV"];
    assert.throws(() => ruumUser(), RuumConfigError);
    assert.throws(() => withRuumUser(() => undefined), RuumConfigError);
  } finally {
    process.env = saved;
  }
});
