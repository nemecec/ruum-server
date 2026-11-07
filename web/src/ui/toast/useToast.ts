import { createContext, useContext } from 'react'

export type ToastOptions = {
  /** Undoes what the toast reports. The toast shows "Undo" and then what the undo did. */
  undo?: () => Promise<string | void>
  /** One more action, for example "Invite people". */
  action?: { label: string; run: () => void }
  /** A failure: an alert for screen readers, and the toast stays until it is closed. */
  error?: boolean
}

export type ToastApi = { toast: (text: string, options?: ToastOptions) => void }

export const ToastContext = createContext<ToastApi>({ toast: () => {} })

/** Shows toasts: short messages at the bottom that go away by themselves (design-system 4.13). */
export function useToast(): ToastApi {
  return useContext(ToastContext)
}
