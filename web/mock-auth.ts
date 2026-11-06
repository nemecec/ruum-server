// A Vite plugin for `npm run dev` with the mock API. It plays the server's sign-in redirects
// (`loginStart` and `loginCallback`), which are full page loads that the mock API in the page
// does not see.

import type { Plugin } from 'vite'

/** A relative path in the UI, like the `return_to` rule of the contract (`^/[^/\\]`). */
function safeReturnTo(value: string | null): string {
  return value && /^\/[^/\\]/.test(value) ? value : '/'
}

export function mockAuth(): Plugin {
  return {
    name: 'ruum-mock-auth',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = new URL(req.url ?? '/', 'http://localhost')
        if (url.pathname === '/v1/auth/login') {
          // loginStart: the real server redirects to the identity provider, which redirects to
          // loginCallback. The mock skips the provider.
          const state = encodeURIComponent(safeReturnTo(url.searchParams.get('return_to')))
          res.statusCode = 302
          res.setHeader('Location', `/v1/auth/callback?code=mock&state=${state}`)
          res.end()
          return
        }
        if (url.pathname === '/v1/auth/callback') {
          const error = url.searchParams.get('error')
          res.statusCode = 302
          if (error) {
            res.setHeader('Location', `/signin?error=${encodeURIComponent(error)}`)
          } else {
            res.setHeader('Set-Cookie', [
              'ruum_mock_session=1; Path=/; SameSite=Lax',
              `ruum_csrf=${crypto.randomUUID()}; Path=/; SameSite=Lax`,
            ])
            res.setHeader('Location', safeReturnTo(url.searchParams.get('state')))
          }
          res.end()
          return
        }
        next()
      })
    },
  }
}
