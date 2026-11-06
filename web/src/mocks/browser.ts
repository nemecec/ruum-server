// Starts the mock API in the browser (`npm run dev`). Wave B runs the UI against the real server
// with `npm run dev:server`.
//
// The mock wraps `fetch` (the API client looks it up on each call) instead of using the MSW
// service worker: it needs no generated worker file and works in browsers that block service
// workers on localhost.

import { getResponse } from 'msw'
import { db } from './db'
import { handlers, setMockLatency } from './handlers'

export function startMockApi(): void {
  // The sign-in redirects are full page loads, which `fetch` does not see. The dev server answers
  // them (mock-auth.ts) and sets this cookie; the mock API reads it here.
  db.signedIn = document.cookie.split('; ').includes('ruum_mock_session=1')
  // The CSRF cookie of the session (double submit); the mock API checks the Ruum-Csrf header.
  if (db.signedIn && !document.cookie.split('; ').some((c) => c.startsWith('ruum_csrf='))) {
    document.cookie = `ruum_csrf=${crypto.randomUUID()}; Path=/; SameSite=Lax`
  }
  setMockLatency(150)
  const realFetch = globalThis.fetch.bind(globalThis)
  globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init)
    if (new URL(request.url).pathname.startsWith('/v1/')) {
      const response = await getResponse(handlers, request)
      if (response) return response
      console.warn(`[mock API] no handler for ${request.method} ${request.url}`)
      return Response.json({ error: { code: 'not_implemented', message: 'The mock API has no handler for this call.' } }, { status: 501 })
    }
    return realFetch(input, init)
  }
  console.info('[mock API] on: the UI runs without the server. Use `npm run dev:server` for the real one.')
}
