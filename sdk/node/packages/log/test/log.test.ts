import { test } from "node:test";
import assert from "node:assert/strict";
import { createLogger, HIDDEN, type Sink } from "../src/index.js";

function capture(): { sink: Sink; lines: () => Record<string, unknown>[]; raw: () => string } {
  let out = "";
  return {
    sink: { write: (s: string) => (out += s) },
    lines: () => out.trim().split("\n").filter(Boolean).map((l) => JSON.parse(l) as Record<string, unknown>),
    raw: () => out,
  };
}

const fixed = () => new Date("2026-11-19T10:00:00.000Z");

test("one JSON object per line with the template as its own field", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed });
  log.info("order {orderId} paid", { orderId: "o-17" });
  log.warn("plain text");
  assert.ok(c.raw().endsWith("\n"));
  assert.equal(c.raw().split("\n").length, 3);
  const [a, b] = c.lines();
  assert.deepEqual(a, {
    time: "2026-11-19T10:00:00.000Z",
    severity_number: 9,
    severity_text: "INFO",
    template: "order {orderId} paid",
    fields: { orderId: "o-17" },
  });
  assert.equal(b?.["severity_number"], 13);
  assert.deepEqual(b?.["fields"], {});
});

test("no field value is put into the template", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed });
  log.info("hello {name}", { name: "Mari Tamm" });
  const [a] = c.lines();
  assert.equal(a?.["template"], "hello {name}");
  assert.ok(!String(a?.["template"]).includes("Mari"));
});

test("denied field names are written as the hidden marker, also nested, also in child loggers", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed, fields: { apiKey: "base-value-1" } });
  log.info("login {user}", {
    user: "u1",
    password: "pw-value-2",
    nested: { access_token: "tok-value-3", ok: 1, deeper: [{ "set-cookie": "c-value-4" }] },
  });
  log.child({ sessionId: "s-value-5" }).error("boom");
  const raw = c.raw();
  for (const v of ["base-value-1", "pw-value-2", "tok-value-3", "c-value-4", "s-value-5"]) {
    assert.ok(!raw.includes(v), `${v} must not be written`);
  }
  const [a, b] = c.lines();
  const f = a?.["fields"] as Record<string, any>;
  assert.equal(f["password"], HIDDEN);
  assert.equal(f["apiKey"], HIDDEN);
  assert.equal(f["nested"]["access_token"], HIDDEN);
  assert.equal(f["nested"]["ok"], 1);
  assert.equal(f["nested"]["deeper"][0]["set-cookie"], HIDDEN);
  assert.equal(f["user"], "u1");
  assert.equal((b?.["fields"] as Record<string, unknown>)["sessionId"], HIDDEN);
});

test("a denied getter is never read", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed });
  let read = false;
  const fields = Object.defineProperty({}, "token", {
    enumerable: true,
    get() {
      read = true;
      return "x";
    },
  });
  log.info("t", fields);
  assert.equal(read, false);
});

test("levels follow OTel severities and the minimum level", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed, level: "warn" });
  log.trace("a");
  log.debug("b");
  log.info("c");
  log.warn("d");
  log.error("e");
  log.fatal("f");
  assert.deepEqual(
    c.lines().map((l) => [l["severity_text"], l["severity_number"]]),
    [["WARN", 13], ["ERROR", 17], ["FATAL", 21]],
  );
  const all = capture();
  const t = createLogger({ sink: all.sink, now: fixed, level: "trace" });
  t.trace("a");
  t.debug("b");
  assert.deepEqual(all.lines().map((l) => l["severity_number"]), [1, 5]);
});

test("values JSON cannot hold, cycles and errors are written safely", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed });
  const cyc: Record<string, unknown> = { a: 1 };
  cyc["self"] = cyc;
  log.error("failed {err}", { err: new Error("bad thing"), big: 10n, cyc, nan: NaN, u: undefined });
  const [a] = c.lines();
  const f = a?.["fields"] as Record<string, any>;
  assert.equal(f["err"]["message"], "bad thing");
  assert.equal(f["big"], "10");
  assert.equal(f["cyc"]["self"], "[circular]");
  assert.equal(f["nan"], "NaN");
  assert.equal(f["u"], null);
});

test("an error's name and message are cleaned like any field", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed });
  const odd = new Error("failed");
  (odd as unknown as { name: unknown }).name = { access_token: "tok-secret-1" };
  const oddMessage = new Error("x");
  (oddMessage as unknown as { message: unknown }).message = { password: "pw-secret-2" };
  log.error("failed", { err: odd, err2: oddMessage, plain: new TypeError("plain one") });
  const raw = c.raw();
  assert.ok(!raw.includes("tok-secret-1"));
  assert.ok(!raw.includes("pw-secret-2"));
  const f = c.lines()[0]?.["fields"] as Record<string, any>;
  assert.equal(f["err"]["name"]["access_token"], HIDDEN);
  assert.equal(f["err2"]["message"]["password"], HIDDEN);
  // Control: a normal error keeps its name and message.
  assert.equal(f["plain"]["name"], "TypeError");
  assert.equal(f["plain"]["message"], "plain one");
});

test("a field named __proto__ stays in the line", () => {
  const c = capture();
  const log = createLogger({ sink: c.sink, now: fixed });
  log.info("x", JSON.parse('{"__proto__":{"a":1},"b":2}'));
  const f = c.lines()[0]?.["fields"] as Record<string, any>;
  assert.deepEqual(Object.keys(f), ["__proto__", "b"]);
});
