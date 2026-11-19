import { test, describe, it, after } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RuleTester } from "eslint";
import rule from "../src/no-secret-log-fields.js";
import plugin, { secretNamesFromManifest } from "../src/index.js";

// RuleTester reports through the test framework's describe/it.
(RuleTester as unknown as { describe: unknown; it: unknown; afterAll: unknown }).describe = describe;
(RuleTester as unknown as { it: unknown }).it = it;
(RuleTester as unknown as { afterAll: unknown }).afterAll = after;

const tester = new RuleTester({ languageOptions: { ecmaVersion: 2024, sourceType: "module" } });

const IMPORT = 'import { log } from "@ruum/log";\n';

tester.run("no-secret-log-fields", rule, {
  valid: [
    { code: `${IMPORT}log.info("order {orderId} paid", { orderId, amount: 3 });` },
    { code: `${IMPORT}log.info("author {author}", { author: "x", keyboard: 1, passenger: 2 });` },
    { code: `console.log("status", { status: 200, route: "/a" });` },
    { code: `console.log(process.env.NODE_ENV, process.env.PORT);` },
    // Not a logger: another library's `info` with a password field is out of scope.
    { code: `const db = makeDb(); db.info({ password: "x" });` },
    { code: `import { log } from "other-log"; log.info("x", { password: 1 });` },
    // A secret name used as a value is fine; only keys and process.env values count.
    { code: `${IMPORT}log.info("rotated {name}", { name: "STRIPE_KEY" });`, options: [{ secrets: ["STRIPE_KEY"], manifest: false }] },
    // A computed key that is not a literal cannot be judged.
    { code: `${IMPORT}log.info("x", { [k]: 1 });` },
    { code: `${IMPORT}const f = { orderId: 1 }; log.info("x", f);` },
  ],
  invalid: [
    {
      code: `${IMPORT}log.info("login {user}", { user, password });`,
      errors: [{ messageId: "deniedField", data: { name: "password" } }],
    },
    {
      code: `console.log("auth", { apiKey: k, "set-cookie": c, nested: { access_token: t } });`,
      errors: [{ messageId: "deniedField" }, { messageId: "deniedField" }, { messageId: "deniedField" }],
    },
    {
      // Aliased import and a child logger.
      code: `import { log as l } from "@ruum/log";\nl.child({ sessionId }).warn("x", { userPassword: p });`,
      errors: [{ messageId: "deniedField", data: { name: "sessionId" } }, { messageId: "deniedField", data: { name: "userPassword" } }],
    },
    {
      code: `import ruumLog from "@ruum/log";\nruumLog.error("x", { token: t });`,
      errors: [{ messageId: "deniedField" }],
    },
    {
      code: `import * as rl from "@ruum/log";\nrl.log.debug("x", { client_secret: s }); const lg = rl.createLogger({ fields: { jwt } }); lg.info("y", { cvv });`,
      errors: [{ messageId: "deniedField" }, { messageId: "deniedField" }, { messageId: "deniedField" }],
    },
    {
      code: `import { createLogger as mk } from "@ruum/log";\nconst a = mk(); const b = a.child({}); b.fatal("x", { privateKey });`,
      errors: [{ messageId: "deniedField", data: { name: "privateKey" } }],
    },
    {
      // Spread objects.
      code: `${IMPORT}const extra = { secret: s }; log.info("x", { ...extra, ok: 1 }); log.info("y", { ...{ pwd } });`,
      errors: [{ messageId: "deniedField", data: { name: "secret" } }, { messageId: "deniedField", data: { name: "pwd" } }],
    },
    {
      // Computed keys that are string literals and plain template literals.
      code: `${IMPORT}log.info("x", { ["authorization"]: h, [\`x-api-key\`]: k });`,
      errors: [{ messageId: "deniedField" }, { messageId: "deniedField" }],
    },
    {
      // A manifest secret, matched across naming styles.
      code: `${IMPORT}log.info("pay", { stripeKey: k, STRIPE_KEY: k2 });`,
      options: [{ secrets: ["STRIPE_KEY"], manifest: false }],
      errors: [
        { messageId: "manifestSecret", data: { name: "stripeKey", secret: "STRIPE_KEY" } },
        { messageId: "manifestSecret", data: { name: "STRIPE_KEY", secret: "STRIPE_KEY" } },
      ],
    },
    {
      code: `console.log(process.env); console.error("env", { env: process.env });`,
      errors: [{ messageId: "processEnv" }, { messageId: "processEnv" }],
    },
    {
      code: `${IMPORT}log.info("x", { k: process.env.STRIPE_KEY, d: process.env["DB_PASSWORD"], r: process.env.RUUM_SECRET_X });`,
      options: [{ secrets: ["STRIPE_KEY"], manifest: false }],
      errors: [{ messageId: "envSecret" }, { messageId: "envSecret" }, { messageId: "envSecret" }],
    },
    {
      // Template literals and concatenation in console calls; JSON.stringify; an alias of process.env.
      code: "const env = process.env;\nconsole.log(`key ${process.env.API_TOKEN}`); console.log('k=' + process.env.MY_SECRET); console.log(JSON.stringify(env));",
      // Reports are sorted by place: the alias is reported where process.env is read.
      errors: [{ messageId: "processEnv", line: 1 }, { messageId: "envSecret" }, { messageId: "envSecret" }],
    },
    {
      code: `console.info({ ...process.env });`,
      errors: [{ messageId: "processEnv" }],
    },
  ],
});

describe("the manifest", () => {
  it("reads the short and the long form of secrets", () => {
    assert.deepEqual(secretNamesFromManifest("name: a\nsecrets:\n  - STRIPE_KEY\n  - 'MAIL_PASS'\nroles: []\n"), ["STRIPE_KEY", "MAIL_PASS"]);
    assert.deepEqual(
      secretNamesFromManifest("secrets:\n  SESSION_KEY:\n    generate: random-32-bytes\n    rotate: 90d\n  STRIPE_KEY: { entered: true }\nbindings: {}\n"),
      ["SESSION_KEY", "STRIPE_KEY"],
    );
    assert.deepEqual(secretNamesFromManifest("secrets: [A_KEY, \"B_KEY\"]  # c\n"), ["A_KEY", "B_KEY"]);
    assert.deepEqual(secretNamesFromManifest("secrets: {X_KEY: {generate: random-16-bytes}, Y: {entered: true}}\n"), ["X_KEY", "Y"]);
    assert.deepEqual(secretNamesFromManifest("name: none\n"), []);
  });

});

// The rule finds app.yaml above the linted file.
const manifestDir = mkdtempSync(join(tmpdir(), "ruum-o5-lint-"));
writeFileSync(join(manifestDir, "app.yaml"), "secrets:\n  - SHOP_API_PIN_CODE\n  - WEBHOOK_HMAC\n");
mkdirSync(join(manifestDir, "src", "lib"), { recursive: true });
const lintedFile = join(manifestDir, "src", "lib", "pay.js");
after(() => rmSync(manifestDir, { recursive: true, force: true }));
tester.run("manifest lookup", rule, {
  valid: [{ code: `console.log({ webhook: 1 });`, filename: lintedFile }],
  invalid: [
    {
      code: `console.log({ webhookHmac: h });`,
      filename: lintedFile,
      errors: [{ messageId: "manifestSecret", data: { name: "webhookHmac", secret: "WEBHOOK_HMAC" } }],
    },
  ],
});

test("the plugin exports the rule and a recommended config", () => {
  assert.equal(plugin.rules["no-secret-log-fields"], rule);
  assert.deepEqual((plugin.configs["recommended"] as { rules: unknown }).rules, { "ruum/no-secret-log-fields": "error" });
});
