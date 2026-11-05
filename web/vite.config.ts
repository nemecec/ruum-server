import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The core service listens on 127.0.0.1:8490 (RUUM_BIND). In development, Vite serves the UI on
// port 5173 and forwards API calls there, so the browser sees one origin.
const api = process.env.RUUM_API_URL ?? 'http://127.0.0.1:8490'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      '/v1': api,
      '/healthz': api,
      '/.well-known': api,
    },
  },
})
