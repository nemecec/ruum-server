import { test } from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import type { AddressInfo } from "node:net";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { isRuumInternal, readWorkloadToken, runtimeFetch, workloadAuthHeaders, WorkloadTokenError } from "../src/workload.js";

function tokenDir(): { env: NodeJS.ProcessEnv; file: string; done: () => void } {
  const dir = mkdtempSync(join(tmpdir(), "ruum-o5-wl-"));
  const file = join(dir, "token");
  return { env: { RUUM_TOKEN_FILE: file }, file, done: () => rmSync(dir, { recursive: true, force: true }) };
}

test("readWorkloadToken reads the file fresh on every call", () => {
  const t = tokenDir();
  try {
    writeFileSync(t.file, "wl-first\n");
    assert.equal(readWorkloadToken(t.env), "wl-first");
    writeFileSync(t.file, "wl-renewed");
    assert.equal(readWorkloadToken(t.env), "wl-renewed");
    writeFileSync(t.file, "two tokens");
    assert.throws(() => readWorkloadToken(t.env), WorkloadTokenError);
    rmSync(t.file);
    assert.throws(() => readWorkloadToken(t.env), (e: Error) => e instanceof WorkloadTokenError && !e.message.includes(t.file));
  } finally {
    t.done();
  }
});

test("the workload token goes only to ruum.internal", () => {
  for (const u of ["http://ruum.internal/v1/self", "http://RUUM.internal/x", "http://db.api.ruum.internal/", "https://ruum.internal/"]) {
    assert.ok(isRuumInternal(u), u);
  }
  for (const u of [
    "http://evil.example/",
    "http://ruum.internal.evil.example/",
    "http://evilruum.internal/",
    "http://ruum.internal@evil.example/",
    "http://user:pw@ruum.internal/",
    "ftp://ruum.internal/",
    "http://127.0.0.1/",
    "not a url",
  ]) {
    assert.ok(!isRuumInternal(u), u);
  }
  const t = tokenDir();
  try {
    writeFileSync(t.file, "wl-value-77");
    assert.deepEqual(workloadAuthHeaders("http://ruum.internal/v1/self", t.env), { authorization: "Bearer wl-value-77" });
    assert.throws(() => workloadAuthHeaders("http://evil.example/", t.env), (e: Error) => e instanceof WorkloadTokenError && !e.message.includes("wl-value-77"));
  } finally {
    t.done();
  }
});

test("runtimeFetch never sends the token to another host", async () => {
  const t = tokenDir();
  let hits = 0;
  const other = http.createServer((_req, res) => {
    hits++;
    res.end("x");
  });
  await new Promise<void>((r) => other.listen(0, "127.0.0.1", r));
  const port = (other.address() as AddressInfo).port;
  const realFetch = globalThis.fetch;
  const sent: { url: string; auth: string | null; redirect: string | undefined }[] = [];
  try {
    writeFileSync(t.file, "wl-value-88");
    await assert.rejects(runtimeFetch(`http://127.0.0.1:${port}/v1/self`, {}, t.env), WorkloadTokenError);
    await assert.rejects(runtimeFetch("//127.0.0.1/v1/self", {}, t.env), WorkloadTokenError);
    assert.equal(hits, 0, "the other host got no request");
    // Positive control: a path goes to ruum.internal with the token (fetch replaced, no network).
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      sent.push({ url: String(input), auth: new Headers(init?.headers).get("authorization"), redirect: init?.redirect });
      return new Response("{}", { status: 200 });
    }) as typeof fetch;
    await runtimeFetch("/v1/self", { headers: { authorization: "Bearer caller-set" } }, t.env);
    assert.deepEqual(sent, [{ url: "http://ruum.internal/v1/self", auth: "Bearer wl-value-88", redirect: "manual" }]);
  } finally {
    globalThis.fetch = realFetch;
    other.close();
    t.done();
  }
});
