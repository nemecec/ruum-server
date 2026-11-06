import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'
import { mockAuth } from './mock-auth.ts'

// The core service listens on 127.0.0.1:8490 (RUUM_BIND). With `npm run dev:server`, Vite serves the
// UI on port 5173 and forwards API calls there, so the browser sees one origin. With `npm run dev`
// (mode `mock`), the mock API in src/mocks answers instead, and no server is needed.
const api = process.env.RUUM_API_URL ?? 'http://127.0.0.1:8490'

export default defineConfig(({ mode }) => {
  const mock = mode === 'mock'
  return {
    plugins: [react(), ...(mock ? [mockAuth()] : [])],
    server: {
      port: 5173,
      strictPort: true,
      // The tests read ../contracts/openapi.yaml to check each mock request against the contract.
      fs: { allow: ['.', '../contracts'] },
      proxy: mock
        ? {}
        : {
            '/v1': api,
            '/healthz': api,
            '/.well-known': api,
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
