import type { GlobalProvider } from '@ladle/react'
import { MemoryRouter } from 'react-router'
import '../src/ui/global.css'

// Every story runs inside a router (links and nav items need one), on the page background.
export const Provider: GlobalProvider = ({ children }) => <MemoryRouter>{children}</MemoryRouter>
