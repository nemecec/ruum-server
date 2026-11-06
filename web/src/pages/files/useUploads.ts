import { createContext, useContext, useEffect, useRef } from 'react'
import type { UploadProgress, UploadTarget } from '../../upload/upload'
import type { Node } from './api'

/** One file in the upload list. */
export type UploadItem = {
  id: string
  name: string
  size: number
  progress: UploadProgress
  status: 'waiting' | 'active' | 'done' | 'failed' | 'canceled'
  error?: unknown
  node?: Node
  /** For a new version that the server joined into the version before (coalescing). */
  replaced?: string
}

/** Where one file goes, and what the UI knows to tell the person about it afterwards. */
export type UploadPlan = { file: File; target: UploadTarget; previousAt?: string }


export type DoneListener = (nodes: Node[]) => void

export type UploadsApi = {
  items: UploadItem[]
  /** Adds files to the queue. `folderName` names the folder in the summary toast. */
  add: (plans: UploadPlan[], tenantId: string, folderName?: string) => void
  cancel: (id: string) => void
  clear: () => void
  /** Runs `listener` with the uploaded nodes each time the queue is done. */
  onDone: (listener: DoneListener) => () => void
  busy: boolean
}

export const UploadsContext = createContext<UploadsApi | null>(null)

export function useUploads(): UploadsApi {
  const u = useContext(UploadsContext)
  if (!u) throw new Error('useUploads outside UploadsProvider')
  return u
}

/** Calls `listener` when uploads finish (to reload the folder on the screen). */
export function useUploadsDone(listener: DoneListener): void {
  const { onDone } = useUploads()
  const ref = useRef(listener)
  useEffect(() => {
    ref.current = listener
  }, [listener])
  useEffect(() => onDone((nodes) => ref.current(nodes)), [onDone])
}

