# The app contracts

DRAFT for review (step 3 specify phase, `docs/STEP3.md`). Design: `docs/DESIGN.md`, sections 9.1
to 9.10.

A web app is a typed node of type `webapp` (provider `ruum.webapp`). People and agents manage it
through the REST API (`contracts/openapi.yaml`, tag `apps`). The files here are the two other
sides of an app:

| File | Who talks to whom |
|---|---|
| `gateway.openapi.yaml` | A browser and the **app gateway**, on every app origin (`https://<app>.<tenant>.ruumapps.net`, previews, custom domains): the access check before a request reaches the app, sign-in on the app origin, the paths under `/.ruum/` (browser script, frontend telemetry, the report widget). |
| `runtime.openapi.yaml` | A **running app** and the platform, at `http://ruum.internal` inside the sandbox: who the app is, the holders of an app role, the token exchange to act for the signed-in user, reads of bound files, publishing events. |
| `user-token.schema.json` | The claims of the `Ruum-User` token that the gateway adds to every request (design 9.4: the design calls the header `Platform-User`). The same token format identifies calls from other apps and partners. |
| `delegated-token.schema.json` | The claims of the token that `exchangeAppUserToken` returns: the app acts for the signed-in user at `ruum.internal` for 5 minutes (design 9.4). |
| `../manifests/app.schema.json` | `app.yaml`, the manifest in the deployment archive. |

## The three origins

| Origin | Serves | Cookies |
|---|---|---|
| `ruum.space` (laptop `ruum.localhost`) | the web UI, the REST API, MCP, sign-in | `ruum_session` (HttpOnly) |
| `<app>.<tenant>.ruumapps.net` (laptop `*.ruumapps.localhost`) | apps through the app gateway | `__Host-ruum_app` per app origin (a session ID) |
| `<id>.content.ruumapps.net` | downloads and previews of user files | none |
| `ruum.space/report` (paths `/.ruum-report/v1/`) | the report page: the consent step of the report widget, where app code cannot script it | `__Host-ruum_report` (15 minutes, one report) |

`ruumapps.net` is a separate registered domain with `*.ruumapps.net` on the Public Suffix List,
so every app origin is its own site (design 9.5, decision 1). The tenant slug `content` (and the
other reserved slugs of `docs/STEP3.md`) is never given to a tenant.

## Rules

- **Clients are open to later steps.** Apps and the SDK ignore claims they do not know, accept
  unknown values of `actor` and new `sub` prefixes, and check only the signature, `iss`
  (`RUUM_ISSUER`), `aud` (`RUUM_AUDIENCE`), `typ`, `env` (`RUUM_ENV`) and `exp`. The issuer is the
  instance's origin, so a self-hosted instance (design 7.5) issues valid tokens.
- **Previews have their own audience.** A preview's user token has
  `aud: app:<app-id>/preview/<deployment-id>` and `env: preview`; the live app refuses it, and
  `exchangeAppUserToken` refuses a token whose `aud` or `env` does not match the caller.
- **The gateway's cookies and credentials never reach the app**: `__Host-ruum_app` and
  `__Host-ruum_visit` are removed from `Cookie`, partner keys and agent tokens from
  `Authorization`; an app's `Set-Cookie` for a `ruum` name is dropped. The internal gateway
  removes the caller's workload token before it forwards an app-to-app call.

- **The gateway decides who may use the app.** An app gets only requests that passed the check
  of `use` (or of `open_preview` for a preview), with a fresh `Ruum-User` token. Incoming `Ruum-*`
  headers are always removed first.
- **An app has no credentials in its archive.** Secrets come from OpenBao as environment
  variables; databases through bindings with short-lived roles; the runtime API with a workload
  token that the runtime mounts and renews.
- **An app reaches only what its bindings allow.** Network policy (Cilium in production, a Docker
  network and the egress proxy on the laptop) allows the internal gateway, the bound databases,
  the bound apps and the egress proxy; nothing else.
- **App tokens never work against the REST API** (another `aud` and `typ`), and API tokens never
  work against the runtime API.
