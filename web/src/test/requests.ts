import { server } from '../mocks/node'

export type Seen = { method: string; path: string; headers: Headers; body: unknown }

/** Records the API requests of a test, to check headers and bodies. */
export function recordRequests(): Seen[] {
  const seen: Seen[] = []
  server.events.on('request:start', async ({ request }) => {
    const url = new URL(request.url)
    let body: unknown
    const text = await request.clone().text()
    try {
      body = text ? JSON.parse(text) : undefined
    } catch {
      body = text
    }
    seen.push({ method: request.method, path: url.pathname + url.search, headers: request.headers, body })
  })
  return seen
}
