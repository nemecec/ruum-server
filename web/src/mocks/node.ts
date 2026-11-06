// The mock API for the component tests (Vitest runs in Node with jsdom).

import { setupServer } from 'msw/node'
import { handlers } from './handlers'

export const server = setupServer(...handlers)
