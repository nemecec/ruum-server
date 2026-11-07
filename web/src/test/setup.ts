import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterAll, afterEach, beforeAll, beforeEach, expect } from 'vitest'
import { setSigningOut, setTenant } from '../api/client'
import { forgetSpaceNames } from '../app/sharedSpaces'
import { resetDb } from '../mocks/db'
import { contractViolations, setRequestCheck } from '../mocks/handlers'
import { server } from '../mocks/node'
import { contractCheck } from './contract'

beforeAll(() => {
  server.listen({ onUnhandledRequest: 'error' })
  setRequestCheck(contractCheck)
})
beforeEach(() => {
  // The session cookie is HttpOnly; the CSRF cookie is readable by the page (double submit).
  document.cookie = 'ruum_csrf=test-csrf-token; path=/'
  contractViolations.length = 0
})
afterEach(() => {
  // Every request of the test must be one that the real server takes (contracts/openapi.yaml).
  const violations = contractViolations.splice(0)
  expect(violations, 'requests that break the contract').toEqual([])
  cleanup()
  server.resetHandlers()
  server.events.removeAllListeners()
  resetDb()
  setTenant(null)
  setSigningOut(false)
  forgetSpaceNames()
  localStorage.clear()
})
afterAll(() => server.close())

// jsdom has no modal dialogs yet; this is enough for the tests (the browser traps the focus).
if (!HTMLDialogElement.prototype.showModal) {
  HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement) {
    this.setAttribute('open', '')
  }
  HTMLDialogElement.prototype.close = function close(this: HTMLDialogElement) {
    this.removeAttribute('open')
  }
}
