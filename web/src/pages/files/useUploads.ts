import { useCallback, useEffect, useRef, useState } from 'react'
import { uploadBlob, type UploadProgress, type UploadTarget } from '../../upload/upload'
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
}

type Job = { item: UploadItem; file: File; target: UploadTarget; tenantId: string; controller: AbortController }

/**
 * A queue of chunked uploads, one file at a time. `onDone` runs when the queue is empty again,
 * with the uploaded nodes.
 */
export function useUploads(onDone: (nodes: Node[], failed: number) => void) {
  const [items, setItems] = useState<UploadItem[]>([])
  const queue = useRef<Job[]>([])
  const running = useRef(false)
  const finished = useRef<{ nodes: Node[]; failed: number }>({ nodes: [], failed: 0 })
  const controllers = useRef(new Map<string, AbortController>())
  const onDoneRef = useRef(onDone)
  useEffect(() => {
    onDoneRef.current = onDone
  }, [onDone])

  // Stop the uploads when the page goes away.
  useEffect(() => {
    const all = controllers.current
    return () => all.forEach((c) => c.abort())
  }, [])

  const update = (id: string, patch: Partial<UploadItem>) =>
    setItems((list) => list.map((i) => (i.id === id ? { ...i, ...patch } : i)))

  const run = useCallback(async () => {
    if (running.current) return
    running.current = true
    while (queue.current.length) {
      const job = queue.current.shift()!
      if (job.controller.signal.aborted) continue
      update(job.item.id, { status: 'active' })
      try {
        const res = await uploadBlob(job.file, job.target, {
          tenantId: job.tenantId,
          signal: job.controller.signal,
          onProgress: (progress) => update(job.item.id, { progress }),
        })
        update(job.item.id, { status: 'done', node: res.node, progress: { phase: 'completing', done: 1, total: 1 } })
        finished.current.nodes.push(res.node)
      } catch (error) {
        if (job.controller.signal.aborted) {
          update(job.item.id, { status: 'canceled' })
        } else {
          update(job.item.id, { status: 'failed', error })
          finished.current.failed++
        }
      } finally {
        controllers.current.delete(job.item.id)
      }
    }
    running.current = false
    const done = finished.current
    finished.current = { nodes: [], failed: 0 }
    onDoneRef.current(done.nodes, done.failed)
  }, [])

  /** Adds files to the queue. `target` says where each one goes. */
  const add = useCallback(
    (files: File[], target: (f: File) => UploadTarget, tenantId: string) => {
      const jobs = files.map((file) => {
        const item: UploadItem = {
          id: crypto.randomUUID(),
          name: file.name,
          size: file.size,
          progress: { phase: 'hashing', done: 0, total: file.size },
          status: 'waiting',
        }
        const controller = new AbortController()
        controllers.current.set(item.id, controller)
        return { item, file, target: target(file), tenantId, controller }
      })
      // Finished items of an earlier round leave the list when a new round starts.
      setItems((list) => [...(running.current ? list : list.filter((i) => i.status === 'active' || i.status === 'waiting')), ...jobs.map((j) => j.item)])
      queue.current.push(...jobs)
      void run()
    },
    [run],
  )

  const cancel = useCallback((id: string) => {
    controllers.current.get(id)?.abort(new DOMException('The upload was canceled.', 'AbortError'))
    setItems((list) => list.map((i) => (i.id === id && i.status === 'waiting' ? { ...i, status: 'canceled' } : i)))
  }, [])

  const clear = useCallback(() => setItems((list) => list.filter((i) => i.status === 'active' || i.status === 'waiting')), [])

  return { items, add, cancel, clear, busy: items.some((i) => i.status === 'active' || i.status === 'waiting') }
}
