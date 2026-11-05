# Ruum web UI

React + TypeScript + Vite. The first UI is small (design section 12.3), because agents use the API.

```sh
npm install
npm run gen:api   # regenerate src/api/schema.d.ts from ../contracts/openapi.yaml
npm run dev       # http://localhost:5173, proxies /v1 to the core service on 127.0.0.1:8490
npm run build     # type check and build to dist/
npm run lint
```

- `src/api/schema.d.ts` is generated from the contract. Never edit it by hand; run `npm run gen:api`
  after a contract change and commit the result with it.
- `src/api/client.ts` adds the session cookie, the `Ruum-Tenant` header and the `Ruum-Csrf` header.
- `.npmrc` points npm at the public registry.
- TypeScript is pinned to 5.9, because `openapi-typescript` 7 needs TypeScript 5.
