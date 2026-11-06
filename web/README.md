# Ruum web UI

React + TypeScript + Vite. The first UI is small (design section 12.3), because agents use the API.

```sh
npm ci
npm run dev         # http://localhost:5173 with the mock API (src/mocks); no server needed
npm run dev:server  # http://localhost:5173, proxies /v1 to the core service on 127.0.0.1:8490
npm run gen:api     # regenerate src/api/schema.d.ts from ../contracts/openapi.yaml
npm run build       # type check and build to dist/
npm run lint
npm test            # component tests (Vitest, Testing Library, jsdom) against the mock API
```

## Layout

| Path | What |
|---|---|
| `src/api/schema.d.ts` | Generated from the contract. Never edit it by hand; run `npm run gen:api` after a contract change. |
| `src/api/client.ts` | The `openapi-fetch` client typed by the schema. It adds the `Ruum-Tenant`, `Ruum-Csrf` and `Idempotency-Key` headers. `unwrap()` returns the data or throws `RuumApiError`. |
| `src/app/` | Routes, the session (`getMe`, `listTenants`), the tenant layout with the tenant switcher, sign-in URLs. |
| `src/pages/` | The pages: files, shared with me, trash, search, agents, OAuth consent, account, tenant admin. |
| `src/components/` | Dialog (native `<dialog>`), confirm button, status and error messages, formatting. |
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

## Not built yet (wave B)

- Large uploads: files over 32 MiB need the chunked upload (`createUpload`, `putChunk`,
  `completeUpload`). The UI uploads small files with `createNode` and `putContent`.
- Database and web app pages (steps 2 and 3).
