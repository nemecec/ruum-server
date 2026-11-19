# The Ruum SDK for Node

Three small packages for apps that run on Ruum (card O-5, `docs/STEP3.md`):

| Package | What it does |
|---|---|
| `@ruum/app` | Checks the `Ruum-User` token that the gateway sends with every request. Helpers for Express and `node:http`. Reads the app's workload token for `ruum.internal`. Dev mode and the `ruum-dev-token` CLI. |
| `@ruum/log` | Structured logs: message templates with named fields, one JSON object per line on stdout. |
| `@ruum/eslint-plugin` | The lint rule `ruum/no-secret-log-fields`. |

`@ruum/app` and `@ruum/log` have no runtime dependencies (Ed25519 comes from `node:crypto`).
Node 22 or later. ESM only.

Publishing to npm is a person's step (decision 0006 E-4). Until then the packages are vendored.

## Install as vendored files

Gallery templates and apps copy the built packages into their own tree:

```sh
cd sdk/node && npm ci && npm run build
# copy into the app (for example into vendor/)
mkdir -p my-app/vendor/@ruum
for p in app log eslint-plugin; do
  mkdir -p my-app/vendor/@ruum/$p
  cp -R packages/$p/package.json packages/$p/dist my-app/vendor/@ruum/$p/
done
```

Then in the app's `package.json`:

```json
{
  "dependencies": {
    "@ruum/app": "file:vendor/@ruum/app",
    "@ruum/log": "file:vendor/@ruum/log"
  },
  "devDependencies": {
    "@ruum/eslint-plugin": "file:vendor/@ruum/eslint-plugin"
  }
}
```

Copy only `package.json` and `dist/src/` (the tests in `dist/test/` are not needed).

## The environment

The Ruum runtime sets these variables in the app's sandbox:

| Variable | Meaning |
|---|---|
| `RUUM_ISSUER` | The origin of the Ruum instance (`https://ruum.space`). The token's `iss` must equal it. |
| `RUUM_AUDIENCE` | `app:<app-id>`, or for a preview `app:<app-id>/preview/<deployment-id>`. The token's `aud` must equal it exactly. |
| `RUUM_ENV` | `live` or `preview`. The token's `env` must equal it. |
| `RUUM_TOKEN_FILE` | The workload token file (default `/var/run/ruum/token`). |
| `RUUM_APP_JWKS_FILE` | **Proposal, not in the contract yet.** A file with the gateway's key set. Without it the SDK fetches `<RUUM_ISSUER>/.well-known/app-jwks.json` over HTTPS. |
| `HTTPS_PROXY` | The egress proxy. The key set fetch goes through it. |

A missing `RUUM_ISSUER`, `RUUM_AUDIENCE` or `RUUM_ENV` is a start-up error (`RuumConfigError`).
The SDK never falls back to "accept every token".

## Check the visitor (`@ruum/app`)

With Express:

```js
import express from "express";
import { ruumUser } from "@ruum/app";

const app = express();
app.use(ruumUser()); // reads the environment now: a missing variable stops the app here
app.get("/", (req, res) => {
  if (!req.ruumUser.roles.includes("approver")) return res.status(403).end();
  res.send(`hello ${req.ruumUser.sub}`);
});
```

With plain `node:http`:

```js
import http from "node:http";
import { withRuumUser } from "@ruum/app";

http.createServer(withRuumUser((req, res, user) => res.end(`hello ${user.sub}`))).listen(8080);
```

Without a framework:

```js
import { verifyUserToken, RuumTokenError } from "@ruum/app";

try {
  const user = await verifyUserToken(req.headers["ruum-user"]);
} catch (e) {
  if (e instanceof RuumTokenError) console.log(e.reason); // for example "wrong_audience"
}
```

A request without `Ruum-User`, or with a refused token, gets `401` with
`{"error":{"code":"unauthenticated",...}}`. Identity comes only from `Ruum-User`; the helpers never
read `X-Forwarded-User`, `Authorization` or any other header for it.

A public app lets in visitors without sign-in with `ruumUser({ allowAnonymous: true })`. The
gateway still sends a token for them (`sub: anonymous:<visit>`), so the header is still required;
`user.anonymous` is `true`.

The user object: `sub`, `tenant`, `roles`, `node_roles`, `actor`, `actorKnown`, `anonymous`, and
when present `name`, `email`, `locale`, `on_behalf_of`, plus `claims` (all claims, read-only).
Do not keep your own copy of roles: the gateway makes a new token for each request.

### What the check refuses, and why

The checks run in this order. Each refusal throws `RuumTokenError` with one `reason`. The message
never holds the token, a key or a claim value.

| Check | Reason | Why |
|---|---|---|
| A string, at most 8192 characters | `missing`, `malformed`, `too_large` | Bounds the work for junk input. |
| Three base64url parts, canonical, no padding | `malformed` | One token has one spelling. |
| Header: strict JSON, no duplicate key | `bad_header` | |
| `alg` is exactly `EdDSA` | `alg_not_allowed` | `none`, `HS256` (the public key used as an HMAC secret), `RS256` and a missing `alg` are refused. |
| No `jku`, `jwk`, `x5u`, `x5c`, `crit` in the header; `typ`, if present, is `JWT`; a `kid` | `bad_header` | A token never chooses its own key. These members are never followed. |
| The `kid` is in the key set | `unknown_kid` | An unknown `kid` refreshes the key set at most once per 30 s, so a flood of made-up `kid`s cannot make the app fetch in a loop. |
| The Ed25519 signature | `bad_signature` | Also refuses claims swapped from another token. |
| Claims: strict JSON, no duplicate key | `duplicate_claim` | `"aud"` twice could mean one thing to `JSON.parse` (the last) and another to a different reader. The SDK refuses; it never picks one. |
| `iss` = `RUUM_ISSUER` | `wrong_issuer` | |
| `aud` = `RUUM_AUDIENCE`, exact string | `wrong_audience` | Another app's token, a preview token at the live app and a live token at the preview are refused. Never a prefix match. |
| `typ` = `app_user` | `wrong_type` | The gateway's sealed state and delegated tokens have other types. |
| `env` = `RUUM_ENV` | `wrong_env` | |
| `iat` and `exp` are integers | `bad_time` | A token without `iat` passed the first spike app (decision 0006 G point 2). |
| `exp` after now (no leeway) | `expired` | |
| `exp - iat` from 1 to 60 s | `lifetime_too_long` | The contract's longest life. |
| `iat` at most 5 s in the future | `issued_in_future` | The clock skew between the gateway and the app (`CLOCK_SKEW_SECONDS` = 5, as the test kit's reference verifier). |
| `sub`, `tenant`, `actor`, `roles`, `node_roles` have their types | `bad_claims` | The user object needs them. |

Unknown claims, unknown `actor` values and unknown `sub` prefixes are accepted (the contract is
open for later steps). `user.actorKnown` is `false` for an unknown actor: treat it like
`anonymous` for your own decisions.

### The key set

The SDK takes the gateway's key set from, in this order: a key set given in code (tests), the file
`RUUM_APP_JWKS_FILE` (read at start and again for an unknown `kid`), or an HTTPS GET of
`<RUUM_ISSUER>/.well-known/app-jwks.json`. Only Ed25519 public keys with a `kid` are kept (at most
16; a key set is at most 64 KiB).

Node 22's built-in `fetch` ignores `HTTPS_PROXY` unless the process runs with
`NODE_USE_ENV_PROXY=1`. So the SDK does not use `fetch` for the key set: it opens an HTTP `CONNECT`
tunnel through `HTTPS_PROXY` itself and speaks TLS to the issuer inside it, with normal certificate
checks.

## The workload token (`@ruum/app`)

```js
import { runtimeFetch, readWorkloadToken } from "@ruum/app";

const self = await (await runtimeFetch("/v1/self")).json(); // http://ruum.internal/v1/self
```

`readWorkloadToken()` reads `RUUM_TOKEN_FILE` fresh on every call (the runtime renews it).
`runtimeFetch` and `workloadAuthHeaders(url)` add `Authorization: Bearer <token>` only for
`ruum.internal` and its sub-domains, and refuse any other host before anything is sent.
`runtimeFetch` does not follow redirects.

## Logs (`@ruum/log`)

```js
import { log, createLogger } from "@ruum/log";

log.info("order {orderId} paid", { orderId: order.id });
const billing = createLogger({ fields: { component: "billing" } });
billing.warn("payment {paymentId} retried {attempt} times", { paymentId, attempt });
```

One line per call:

```json
{"time":"2026-11-19T10:00:00.000Z","severity_number":9,"severity_text":"INFO","template":"order {orderId} paid","fields":{"orderId":"o-1"}}
```

- The template is written as it is. No field value is put into it, so the platform classifies each
  field by its name, and the template holds no personal data. Write values as fields, never into
  the template string.
- Levels are OpenTelemetry severities: `trace` 1, `debug` 5, `info` 9, `warn` 13, `error` 17,
  `fatal` 21. The minimum level is `LOG_LEVEL` (default `info`).
- A field whose name is on the platform deny-list (`password`, `token`, `apiKey`, `cookie`,
  `session`, ... from `crates/ruum-core/src/telemetry/denylist.rs`) is written as
  `<hidden secret>`, at any depth. The platform's processor still decides; this is defence in depth.
  A test checks that the SDK's list equals the platform's.

## The lint rule (`@ruum/eslint-plugin`)

```js
// eslint.config.js
import ruum from "@ruum/eslint-plugin";

export default [
  { plugins: { ruum }, rules: { "ruum/no-secret-log-fields": "error" } },
];
```

The rule reports a call of `@ruum/log` (also aliased imports, namespace imports, `createLogger`
and `child`) or `console.*` that logs:

- a field whose key is on the platform deny-list: `log.info("login", { password })`;
- a field whose key matches a secret of the app's manifest (`secrets` in the nearest `app.yaml`):
  `stripeKey` matches `STRIPE_KEY`;
- `process.env`, or `process.env.<NAME>` for a manifest secret, a denied name or `RUUM_SECRET_*`.

It follows spread objects, computed keys that are string literals or plain template literals,
template literals, `+`, `JSON.stringify(...)` and local variables. Options:
`{ secrets: ["EXTRA_NAME"], manifest: false }` (`manifest: false` turns off the `app.yaml` lookup).

## Dev mode

Run and test an app on a laptop without the platform:

```sh
npx ruum-dev-token init                      # makes .ruum-dev/key.pem and .ruum-dev/jwks.json
export RUUM_ISSUER=http://localhost
export RUUM_APP_JWKS_FILE=$PWD/.ruum-dev/jwks.json
export RUUM_ENV=preview
export RUUM_AUDIENCE=app:0190b6c4-a00d-7cc1-9a4b-1f2e3d4c5b7d/preview/0190b6c4-a00d-7cc1-9a4b-0000000000d1
node server.js &
curl -H "Ruum-User: $(npx ruum-dev-token mint --aud "$RUUM_AUDIENCE" --roles member)" http://localhost:8080/
```

In code (tests):

```js
import { initDev, mintDevToken } from "@ruum/app/dev";

initDev();
const token = mintDevToken({ audience: process.env.RUUM_AUDIENCE, roles: ["member"] });
```

- Dev mode refuses to run when `RUUM_ENV` is `live` or `NODE_ENV` is `production`.
- Its issuer is `http://localhost` (a port is allowed) or `http://127.0.0.1`. A real deployment's
  `RUUM_ISSUER` is an `https://` origin, so it refuses every dev token (`wrong_issuer`).
- The key stays in `.ruum-dev/` (mode 0600). The folder gets a `.gitignore` of `*`, so it is never
  committed.

## Develop the SDK

```sh
cd sdk/node
npm ci
npm test         # tsc -b, then node --test on every package
npm run vectors  # writes testdata/user-tokens.json again
```

`testdata/user-tokens.json` holds user tokens in the gateway's format, signed with a test key made
from a phrase. `crates/ruum-gateway/tests/sdk_vectors.rs` signs the same claims with the gateway's
own signer and compares them byte for byte; the SDK's tests verify the same file. A change of the
gateway's token format fails one of the two.

Dev dependencies only: `typescript` 5.9, `@types/node` 24, `eslint` 9 (for `RuleTester`).
