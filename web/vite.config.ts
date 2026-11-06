import react from '@vitejs/plugin-react'
import type { ProxyOptions } from 'vite'
import { defineConfig } from 'vitest/config'
import { mockAuth } from './mock-auth.ts'

// Two ways to run the UI:
//
// - `npm run dev` (mode `mock`): the mock API in src/mocks answers; no server is needed.
// - `npm run dev:server`: Vite serves the UI on http://localhost:5173 and forwards the API to the
//   Ruum server, so the browser sees one origin. The target is `RUUM_API_URL`:
//     http://127.0.0.1:8490   the server on the host (default, `RUUM_BIND`)
//     https://ruum.localhost  the same server through Caddy, the HTTPS front of the laptop stack
//
// Sign-in through Vite: the identity provider (Zitadel) sends the browser back to
// `http://localhost:5173/auth/callback`, a redirect URI that the stack registers for the `ruum-web`
// client. Start the server with `RUUM_OIDC_REDIRECT_URI=http://localhost:5173/auth/callback`; Vite
// forwards that path to `loginCallback` (`/v1/auth/callback`). See README.md.
const api = process.env.RUUM_API_URL ?? 'http://127.0.0.1:8490'
const port = Number(process.env.RUUM_WEB_PORT ?? 5173)

/** The proxy settings of one path of the API. */
function toServer(extra: ProxyOptions = {}): ProxyOptions {
  const https = api.startsWith('https:')
  return {
    target: api,
    // Caddy picks the site by the Host header; the server on 8490 does not care.
    changeOrigin: https,
    // Caddy's certificate comes from its local CA, which Node does not trust.
    secure: false,
    // Login state cookies have `Path=/v1/auth`; the callback comes back on `/auth/callback`.
    cookiePathRewrite: { '/v1/auth': '/' },
    configure(proxy) {
      proxy.on('proxyRes', (res) => {
        // The server marks cookies `Secure` when its public URL is https. Vite serves plain http on
        // localhost; most browsers accept that, Safari does not, so drop the flag here (dev only).
        const cookies = res.headers['set-cookie']
        if (cookies) res.headers['set-cookie'] = cookies.map((c) => c.replace(/;\s*Secure/gi, ''))
      })
    },
    ...extra,
  }
}

export default defineConfig(({ mode }) => {
  const mock = mode === 'mock'
  return {
    plugins: [react(), ...(mock ? [mockAuth()] : [])],
    server: {
      port,
      strictPort: true,
      // IPv4 loopback, where Caddy in Docker (host.docker.internal) can reach Vite; `localhost` is
      // only ::1 on macOS. Open the UI as http://localhost:5173 anyway: the sign-in callback comes
      // back to that name, and cookies belong to the name.
      host: '127.0.0.1',
      // `ruum.localhost` when Caddy forwards the UI to Vite (see README.md).
      allowedHosts: ['localhost', '127.0.0.1', 'ruum.localhost'],
      // The tests read ../contracts/openapi.yaml to check each mock request against the contract.
      fs: { allow: ['.', '../contracts'] },
      proxy: mock
        ? {}
        : {
            '/v1': toServer(),
            '/healthz': toServer(),
            '/.well-known': toServer(),
            '/auth/callback': toServer({ rewrite: (path) => `/v1${path}` }),
          },
    },
    preview: {
      port: 4173,
      proxy: mock
        ? {}
        : {
            '/v1': toServer(),
            '/healthz': toServer(),
            '/.well-known': toServer(),
            '/auth/callback': toServer({ rewrite: (path) => `/v1${path}` }),
          },
    },
    test: {
      environment: 'jsdom',
      setupFiles: ['./src/test/setup.ts'],
      css: false,
      restoreMocks: true,
    },
  }
})
