import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, RouterProvider } from 'react-router'
import { routes } from './app/routes'
import './index.css'

async function main() {
  // `npm run dev` runs with the mock API (mode `mock`); `npm run dev:server` and builds use the server.
  if (import.meta.env.MODE === 'mock') {
    const { startMockApi } = await import('./mocks/browser')
    startMockApi()
  }
  const router = createBrowserRouter(routes)
  createRoot(document.getElementById('root')!).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  )
}

void main()
