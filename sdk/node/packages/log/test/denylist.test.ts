import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { HIDDEN, SEGMENTS, SUBSTRINGS, isDenied, segments } from "../src/denylist.js";

/** The repository root: the first parent folder that holds `crates/ruum-core`. */
function repoRoot(): string {
  let dir = dirname(fileURLToPath(import.meta.url));
  for (let i = 0; i < 12; i++) {
    if (existsSync(join(dir, "crates", "ruum-core"))) return dir;
    dir = dirname(dir);
  }
  throw new Error("the repository root was not found");
}

/** The string items of `pub const <name>: &[&str] = &[ ... ];` in a Rust file. */
function rustList(text: string, name: string): string[] {
  const m = new RegExp(`pub const ${name}: &\\[&str\\] = &\\[([^\\]]*)\\];`).exec(text);
  assert.ok(m, `${name} is in the Rust file`);
  return Array.from((m[1] as string).matchAll(/"([^"]*)"/g), (x) => x[1] as string);
}

test("the SDK's deny-list equals the platform's (crates/ruum-core/src/telemetry/denylist.rs)", () => {
  const text = readFileSync(join(repoRoot(), "crates/ruum-core/src/telemetry/denylist.rs"), "utf8");
  const subs = rustList(text, "SUBSTRINGS");
  const segs = rustList(text, "SEGMENTS");
  assert.ok(subs.length > 5 && segs.length > 5, "the parse found the lists");
  assert.deepEqual([...SUBSTRINGS], subs);
  assert.deepEqual([...SEGMENTS], segs);
  // The separators of the Rust `segments` function are the SDK's too.
  const sep = /matches!\(c, ([^)]*)\)/.exec(text);
  assert.ok(sep, "the Rust separator list is found");
  const rustSeps = Array.from((sep[1] as string).matchAll(/'(.)'/g), (x) => x[1]);
  assert.deepEqual(rustSeps, ["-", "_", ".", " ", "/", ":"]);
});

test("the hidden marker equals the platform's REDACTED", () => {
  const text = readFileSync(join(repoRoot(), "crates/ruum-core/src/runtime/redact.rs"), "utf8");
  const m = /pub const REDACTED: &str = "([^"]*)";/.exec(text);
  assert.ok(m);
  assert.equal(HIDDEN, m[1]);
});

test("the normalisation table of the platform's tests", () => {
  const denied = [
    "password", "Password", "userPassword", "db.password", "X-Api-Key", "x_api_key", "apiKey",
    "api_key", "stripe_token", "access_token", "refreshToken", "client_secret", "private_key",
    "privateKey", "authorization", "Authorization", "authHeader", "cookie", "set-cookie",
    "credential", "credentials", "session", "sessionId", "passwd", "pwd", "jwt", "db.dsn",
    "AWSSecretAccessKey",
  ];
  for (const n of denied) assert.ok(isDenied(n), `${n} must be denied`);
  const allowed = ["order_id", "author", "route", "http.method", "user_id", "amount", "keyboard", "passenger", "status"];
  for (const n of allowed) assert.ok(!isDenied(n), `${n} must not be denied`);
});

test("segments split like the platform", () => {
  assert.deepEqual(segments("userPassword"), ["user", "password"]);
  assert.deepEqual(segments("XMLHttpToken"), ["xml", "http", "token"]);
  assert.deepEqual(segments("X-Api-Key"), ["x", "api", "key"]);
  assert.deepEqual(segments("db.password_v2"), ["db", "password", "v2"]);
  assert.deepEqual(segments("v2Token"), ["v2", "token"]);
  assert.deepEqual(segments("STRIPE_KEY"), ["stripe", "key"]);
});
