# Ruum web UI

React + TypeScript + Vite. The first UI is small (design section 12.3), because agents use the API.

```sh
npm ci
npm run dev         # http://localhost:5173 with the mock API (src/mocks); no server needed
npm run dev:server  # http://localhost:5173, proxies the API to the Ruum server (see below)
npm run gen:api     # regenerate src/api/schema.d.ts from ../contracts/openapi.yaml
npm run build       # type check and build to dist/
npm run lint        # oxlint, stylelint, the design system rules, generated tokens up to date
npm test            # component tests (Vitest, Testing Library, jsdom) against the mock API
npm run tokens      # regenerate src/design/generated/ from the design tokens
npm run ladle       # the component catalogue (Ladle) on http://localhost:61000
```

## Design system

The look is defined once (design 12.5, `docs/ux/design-system.md` section 6):

- **Tokens** in the W3C Design Tokens format: `src/design/tokens/` (`base`, `color.light`,
  `color.dark`). Style Dictionary (`scripts/build-tokens.mjs`) writes `src/design/generated/`
  (`tokens.css`, `tokens.ts`). The output is committed; `npm run lint` fails when it is stale.
- **Components** in `src/ui/`, exported by `src/ui/index.ts`, each with its CSS and Ladle stories.
  Screens (`src/app`, `src/pages`, `src/components`) use only these components: no `className`,
  no `style`, no CSS files, no raw controls (`scripts/check-ui-rules.mjs`).
- **stylelint** (`stylelint.config.mjs`) refuses colour, length, font, shadow, layer and timing
  values outside the token files.
- **Catalogue:** `npm run ladle` / `npm run ladle:build` (to `build-ladle/`). Light and dark with
  the theme button; phone and desktop width; the a11y panel. `src/ui/catalogue.test.tsx` renders
  every story and runs axe in Vitest.

## Against the real server

`npm run dev:server` forwards `/v1`, `/healthz`, `/.well-known` and `/auth/callback` to the server
named by `RUUM_API_URL`:

```sh
npm run dev:server                                     # http://127.0.0.1:8490, the server on the host
RUUM_API_URL=https://ruum.localhost npm run dev:server # the same server through Caddy (TLS)
RUUM_WEB_PORT=5199 npm run dev:server                  # another port for Vite
```

Sign-in goes through Zitadel, which sends the browser back to a registered redirect URI. The stack
registers `http://localhost:5173/auth/callback` for the `ruum-web` client, so start the server with
that callback:

```sh
eval "$(../scripts/dev.sh env)"
RUUM_OIDC_REDIRECT_URI=http://localhost:5173/auth/callback cargo run -p ruum-server
```

Vite forwards `/auth/callback` to `loginCallback` (`/v1/auth/callback`). It rewrites the path of
the sign-in state cookie (`Path=/v1/auth` to `/`) and drops the `Secure` flag of cookies, because
Vite serves plain http on localhost. Without `RUUM_OIDC_REDIRECT_URI`, the server sends Zitadel to
`https://ruum.localhost/v1/auth/callback`, and the session cookie lands on `ruum.localhost`, not on
`localhost:5173`.

To serve the UI at `https://ruum.localhost` itself, Caddy sends `/v1`, `/healthz`, `/.well-known`
and `/mcp` to the server (WebDAV has its own host, `https://dav.ruum.localhost/`) and every other path to Vite (`RUUM_WEB_UPSTREAM` in
`deploy/dev/.env`, default `host.docker.internal:5173`; `deploy/dev/caddy/Caddyfile`). Set it to
the same port as `RUUM_WEB_PORT` and recreate the Caddy container. Caddy also sends the content
domain (`https://<tenant>.content.ruumapps.localhost/c/<token>`, the links of Download and Preview)
to the server. Then the default callback
`https://ruum.localhost/v1/auth/callback` works and no `RUUM_OIDC_REDIRECT_URI` is needed. Vite
listens on 127.0.0.1 so that Caddy in Docker can reach it, and accepts the host `ruum.localhost`.
Caddy reads the Caddyfile at start; reload it after a change.

Operations that the server does not have yet answer `501 not_implemented`; the page shows the
server's message. On `main` after wave A these are all node, tag, search, upload, content and
version operations, so the file pages work only in mock mode until those areas are merged. Sign-in,
account, tenants, members, invitations, agents, OAuth consent, sharing and the audit log use the
real server.

## Uploads and downloads

Every upload is chunked (`src/upload/`): `getChunkingParams`, then the browser cuts the file with
FastCDC 2020 and hashes each chunk and the whole file with BLAKE3, `createUpload` with the chunk
list, `putChunk` for only the chunks in `missing` (three at a time, retries on `429` and `5xx`),
`completeUpload` (it asks `getUpload` again on `chunk_missing`), and `abortUpload` on cancel. The
chunker is a port of the Rust crate `fastcdc` 5.0.0 (`v2020`, normalization level 1), so the browser
and the server cut a file at the same places and dedup works; `src/test/upload/vectors.ts` holds
test vectors from the Rust crates. The file is read in 8 MiB slices, never as a whole.
"Upload new version" sends `target: new_version` with the current head as `base_version`, so a
concurrent write fails with `version_mismatch` instead of being lost.

`src/api/uploads.ts` corrects two generated types: openapi-typescript renders the `allOf` with the
discriminated `UploadTarget` of `CreateUploadRequest` and `Upload` as `target: "<schema name>"`.

Downloads and previews use `getContentUrl` (the content domain), never the API-origin download:
"Download" with `disposition=attachment`, "Preview" with `disposition=inline` in a new tab.

## Layout

| Path | What |
|---|---|
| `src/api/schema.d.ts` | Generated from the contract. Never edit it by hand; run `npm run gen:api` after a contract change. |
| `src/api/client.ts` | The `openapi-fetch` client typed by the schema. It adds the `Ruum-Tenant`, `Ruum-Csrf` and `Idempotency-Key` headers. `unwrap()` returns the data or throws `RuumApiError`. |
| `src/app/` | Routes, the session (`getMe`, `listTenants`), the tenant layout with the tenant switcher, sign-in URLs. |
| `src/pages/` | The pages: files, shared with me, trash, search, agents, OAuth consent, account, tenant admin. |
| `src/upload/` | Chunked uploads: FastCDC (`fastcdc.ts`, `gear.ts`), chunking and hashing (`chunker.ts`), the upload flow (`upload.ts`). |
| `src/components/` | Formatting helpers and the offline banner. |
| `src/ui/` | The component library (design system): layout, text, buttons, fields, dialogs, notes, tables, lists, navigation, menus, toasts, icons, the app shell. |
| `src/design/` | The design tokens (`tokens/`) and their generated CSS and TypeScript (`generated/`). |
| `.ladle/` | The Ladle configuration of the component catalogue. |
| `src/mocks/` | The mock API: seed data (`db.ts`) and MSW handlers (`handlers.ts`), both typed by the schema. |
| `mock-auth.ts` | Vite plugin for `npm run dev`: plays `loginStart` and `loginCallback` (full page redirects). |
| `src/test/` | Test setup, helpers and the flow tests. |

## Sign-in

The UI never handles tokens. A page without a session goes to `/signin?return_to=<page>`. The
buttons there are links to `loginStart` (`/v1/auth/login?return_to=...&idp=...`). The server
redirects to the identity provider, which returns to `loginCallback`; that sets the session cookie
and redirects to `return_to`. Any later `401` goes back to sign-in. In mock mode the dev server
answers both redirects and sets the cookie `ruum_mock_session=1`; "Sign out" clears it.

## Mock API

`npm run dev` sets the Vite mode `mock`. The page then wraps `fetch` and answers `/v1/...` with the
handlers in `src/mocks/handlers.ts` (MSW `getResponse`, no service worker). The data lives in
memory and resets on a page load. The tests use the same handlers through `msw/node`.

The paths, bodies and answers of the handlers are typed by `schema.d.ts`, so a contract change
fails the type check there first. In the tests, each request is also checked at runtime against
`../contracts/openapi.yaml` (`src/test/contract.ts`): the JSON body against its schema, rules that
the contract gives only in words (`UndoRequest` needs `since` or `seqs`), and whether the operation
takes the `Ruum-Tenant` header. The mock refuses writes without a matching `Ruum-Csrf` header. A
test fails when any of its requests breaks the contract.

The client sends `Ruum-Tenant` only to operations that declare it (`takesTenantHeader` in
`src/api/client.ts`); `listSharedWithMe`, `/v1/me`, `/v1/tenants` and OAuth consent get none.
Shared items belong to their own tenant (`node.tenant_id`), and calls on them name that tenant.

The seed has a third tenant where the user is a guest: the root and "Projects" are hidden
(`getRoot` answers 403, `getNode` 404), and only "Engine drawings" is shared. Lists are paged
(the mock cursor is an offset; the default limit is 100), and the UI shows "Load more".

## Not built yet

- Database and web app pages (steps 2 and 3).
- Drag and drop of files and folder uploads.
