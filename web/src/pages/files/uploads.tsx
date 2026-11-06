// The upload queue of the whole session (design-system 4.5): it lives above the pages, so uploads
// go on and stay visible when the person opens Agents, Trash or another folder. The panel sits at
// the bottom right.

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { errorText } from '../../api/errors'
import { count, formatBytes, formatWhen } from '../../components/format'
import { useToast } from '../../components/useToast'
import { Dialog, ErrorNote } from '../../components/ui'
import { uploadBlob, type UploadProgress } from '../../upload/upload'
import type { Node } from './api'
import { UploadsContext, type DoneListener, type UploadItem, type UploadPlan } from './useUploads'

type Job = UploadPlan & { item: UploadItem; tenantId: string; controller: AbortController; folderName?: string }

export function UploadsProvider({ children }: { children: ReactNode }) {
  const { toast } = useToast()
  const [items, setItems] = useState<UploadItem[]>([])
  const queue = useRef<Job[]>([])
  const running = useRef(false)
  const round = useRef<{ nodes: Node[]; failed: number; folders: Set<string>; replaced: UploadItem[] }>({
    nodes: [],
    failed: 0,
    folders: new Set(),
    replaced: [],
  })
  const controllers = useRef(new Map<string, AbortController>())
  const listeners = useRef(new Set<DoneListener>())
  const busy = items.some((i) => i.status === 'active' || i.status === 'waiting')

  // Warn before the page goes away while uploads run.
  useEffect(() => {
    if (!busy) return
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault()
      e.returnValue = ''
    }
    window.addEventListener('beforeunload', warn)
    return () => window.removeEventListener('beforeunload', warn)
  }, [busy])

  // Stop the uploads when the session ends (sign-out).
  useEffect(() => {
    const all = controllers.current
    return () => all.forEach((c) => c.abort())
  }, [])

  const update = (id: string, patch: Partial<UploadItem>) => setItems((list) => list.map((i) => (i.id === id ? { ...i, ...patch } : i)))

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
        // A new version by the same person within the coalescing window joins the version before
        // (design 5.3): the head version stays the same. Say so instead of hiding it.
        const base = job.target.target === 'new_version' ? job.target.base_version : null
        const replaced = base && res.node.head_version === base ? (job.previousAt ?? res.node.updated_at) : undefined
        const done: Partial<UploadItem> = { status: 'done', node: res.node, replaced, progress: { phase: 'completing', done: 1, total: 1 } }
        update(job.item.id, done)
        round.current.nodes.push(res.node)
        if (job.folderName) round.current.folders.add(job.folderName)
        if (replaced) round.current.replaced.push({ ...job.item, ...done })
      } catch (error) {
        if (job.controller.signal.aborted) {
          update(job.item.id, { status: 'canceled' })
        } else {
          update(job.item.id, { status: 'failed', error })
          round.current.failed++
        }
      } finally {
        controllers.current.delete(job.item.id)
      }
    }
    running.current = false
    const r = round.current
    round.current = { nodes: [], failed: 0, folders: new Set(), replaced: [] }
    if (r.nodes.length || r.failed) toast(summary(r))
    listeners.current.forEach((l) => l(r.nodes))
  }, [toast])

  const add = useCallback(
    (plans: UploadPlan[], tenantId: string, folderName?: string) => {
      const jobs: Job[] = plans.map((p) => {
        const item: UploadItem = {
          id: crypto.randomUUID(),
          name: p.file.name,
          size: p.file.size,
          progress: { phase: 'hashing', done: 0, total: p.file.size },
          status: 'waiting',
        }
        const controller = new AbortController()
        controllers.current.set(item.id, controller)
        return { ...p, item, tenantId, controller, folderName }
      })
      // Finished items of an earlier round leave the list when a new round starts.
      setItems((list) => [
        ...(running.current ? list : list.filter((i) => i.status === 'active' || i.status === 'waiting')),
        ...jobs.map((j) => j.item),
      ])
      queue.current.push(...jobs)
      void run()
    },
    [run],
  )

  const cancel = useCallback((id: string) => {
    controllers.current.get(id)?.abort(new DOMException('The upload was cancelled.', 'AbortError'))
    setItems((list) => list.map((i) => (i.id === id && i.status === 'waiting' ? { ...i, status: 'canceled' } : i)))
  }, [])

  const clear = useCallback(() => setItems((list) => list.filter((i) => i.status === 'active' || i.status === 'waiting')), [])

  const onDone = useCallback((listener: DoneListener) => {
    listeners.current.add(listener)
    return () => {
      listeners.current.delete(listener)
    }
  }, [])

  return (
    <UploadsContext.Provider value={{ items, add, cancel, clear, onDone, busy }}>
      {children}
      <UploadPanel items={items} onCancel={cancel} onClear={clear} />
    </UploadsContext.Provider>
  )
}

/** The toast at the end of a round (copy.md 3.2). */
function summary(r: { nodes: Node[]; failed: number; folders: Set<string>; replaced: UploadItem[] }): string {
  const n = r.nodes.length
  const folder = r.folders.size === 1 ? [...r.folders][0] : null
  let text: string
  if (n === 0) text = `${count(r.failed, 'file')} didn't upload.`
  else if (n === 1 && !r.failed) text = `Uploaded ${r.nodes[0].name}.`
  else if (r.failed) text = `Uploaded ${count(n, 'file')}. ${r.failed} didn't upload.`
  else text = folder ? `Uploaded ${count(n, 'file')} to ${folder}.` : `Uploaded ${count(n, 'file')}.`
  if (r.replaced.length === 1) {
    const it = r.replaced[0]
    text += ` It replaced your version from ${formatWhen(it.replaced!).toLowerCase()}, because Ruum keeps one version for changes made within 10 minutes.`
  } else if (r.replaced.length > 1) {
    text += ` ${r.replaced.length} of them replaced your versions from the last 10 minutes.`
  }
  return text
}

const phaseLabels: Record<UploadProgress['phase'], string> = {
  hashing: 'Checking',
  uploading: 'Uploading',
  completing: 'Finishing',
}

function statusText(item: UploadItem): string {
  switch (item.status) {
    case 'waiting':
      return 'Waiting'
    case 'done': {
      if (item.replaced) return 'Done, joined into the version from the last 10 minutes'
      if (item.node && item.node.name !== item.name) return `Done, saved as ${item.node.name}`
      return 'Done'
    }
    case 'canceled':
      return 'Cancelled'
    case 'failed':
      return `Didn't upload: ${errorText(item.error, { name: item.name })}`
    case 'active': {
      const { phase, done, total } = item.progress
      if (phase !== 'uploading') return phaseLabels[phase]
      const pct = total ? Math.floor((done / total) * 100) : 0
      return `${phaseLabels[phase]} ${pct}%`
    }
  }
}

function header(items: UploadItem[]): string {
  const open = items.filter((i) => i.status === 'active' || i.status === 'waiting').length
  const failed = items.filter((i) => i.status === 'failed').length
  if (open) return `Uploading ${items.length - open + 1} of ${items.length}`
  if (failed) return failed === 1 ? '1 upload needs you' : `${failed} uploads need you`
  return items.length === 1 ? '1 upload done' : `${items.length} uploads done`
}

/** The upload panel at the bottom right, on every page of the session. */
export function UploadPanel(props: { items: UploadItem[]; onCancel: (id: string) => void; onClear: () => void }) {
  const { items } = props
  const running = items.some((i) => i.status === 'active' || i.status === 'waiting')
  const failed = items.some((i) => i.status === 'failed')
  // When everything is done, the panel folds to its header after a moment, so it does not cover
  // the page. It is open while uploads run and while one needs the person.
  const doneKey = running || failed ? null : items.map((i) => i.id).join(',')
  const [foldedFor, setFoldedFor] = useState<string | null>(null)
  const [manual, setManual] = useState<{ key: string | null; open: boolean } | null>(null)
  useEffect(() => {
    if (!doneKey) return
    const t = setTimeout(() => setFoldedFor(doneKey), 4000)
    return () => clearTimeout(t)
  }, [doneKey])
  const open = manual && manual.key === doneKey ? manual.open : !(doneKey && foldedFor === doneKey)
  if (items.length === 0) return null
  const finished = items.some((i) => i.status !== 'active' && i.status !== 'waiting')
  return (
    <section className="upload-panel" aria-label="Uploads">
      <div className="upload-panel-head">
        <h2 aria-live="polite">{header(items)}</h2>
        <button type="button" className="icon" aria-expanded={open} aria-label={open ? 'Hide uploads' : 'Show uploads'} onClick={() => setManual({ key: doneKey, open: !open })}>
          {open ? '−' : '+'}
        </button>
      </div>
      <div hidden={!open}>
        <>
          <ul>
            {items.map((i) => {
              const { done, total } = i.progress
              const saved = i.node && i.node.name !== i.name ? i.node.name : i.name
              return (
                <li key={i.id} className={`upload upload-${i.status}`}>
                  <div className="upload-head">
                    <span className="upload-name">{saved}</span>
                    <span className="muted small">{formatBytes(i.size)}</span>
                  </div>
                  {i.status === 'active' && i.progress.phase === 'uploading' && (
                    <progress max={total || 1} value={done} aria-label={`Progress of ${i.name}`} />
                  )}
                  <div className="upload-head">
                    <span className={i.status === 'failed' ? 'error small' : 'muted small'} role={i.status === 'failed' ? 'alert' : undefined}>
                      {statusText(i)}
                    </span>
                    {(i.status === 'active' || i.status === 'waiting') && (
                      <button type="button" aria-label={`Cancel upload of ${i.name}`} onClick={() => props.onCancel(i.id)}>
                        Cancel
                      </button>
                    )}
                  </div>
                </li>
              )
            })}
          </ul>
          {finished && (
            <button type="button" className="link small" onClick={props.onClear}>
              Clear finished uploads
            </button>
          )}
        </>
      </div>
    </section>
  )
}

export type ConflictChoice = 'version' | 'keep' | 'skip'

/**
 * Asks once per batch what to do with files whose names are already in the folder (copy.md 3.2):
 * a new version (the default), keep both, or skip. `recent` are the items whose head version is
 * the person's own unnamed save of the last 10 minutes: a new version replaces it (design 5.3).
 * Then the dialog says so, names the choice "Replace", makes "Keep both" the default, and offers
 * to name the current version to keep it.
 */
export function ConflictDialog(props: {
  names: string[]
  recent?: Node[]
  onChoose: (choice: ConflictChoice, label: string) => void | Promise<void>
  onClose: () => void
}) {
  const { names } = props
  const recent = props.recent ?? []
  const [choice, setChoice] = useState<ConflictChoice>(recent.length ? 'keep' : 'version')
  const [label, setLabel] = useState('')
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  const title = names.length === 1 ? `${names[0]} is already here` : `${names.length} items are already here`
  const allRecent = recent.length > 0 && recent.length === names.length
  const versionLabel =
    names.length === 1 && allRecent
      ? `Replace your version from ${formatWhen(recent[0].updated_at).toLowerCase()}`
      : allRecent
        ? 'Replace your versions from the last 10 minutes'
        : 'Upload as a new version'
  return (
    <Dialog title={title} open onClose={props.onClose}>
      <form
        className="stack"
        onSubmit={async (e) => {
          e.preventDefault()
          setBusy(true)
          setError(undefined)
          try {
            await props.onChoose(choice, choice === 'version' ? label.trim() : '')
          } catch (err) {
            setError(err)
            setBusy(false)
          }
        }}
      >
        {names.length > 1 && (
          <ul className="small">
            {names.slice(0, 8).map((n) => (
              <li key={n}>{n}</li>
            ))}
            {names.length > 8 && <li>and {names.length - 8} more</li>}
          </ul>
        )}
        {recent.length > 0 && (
          <p className="warning" role="note">
            {recent.length === 1
              ? `You changed ${recent[0].name} ${formatWhen(recent[0].updated_at).toLowerCase()}.`
              : `You changed ${recent.length} of these files in the last 10 minutes.`}{' '}
            Ruum keeps one version for the changes you make within 10 minutes, so a new version replaces{' '}
            {recent.length === 1 ? 'that version' : 'those versions'}.
          </p>
        )}
        <fieldset className="stack">
          <legend>{names.length === 1 ? 'What should Ruum do?' : `What should Ruum do with all ${names.length}?`}</legend>
          <label className="check">
            <input type="radio" name="conflict" checked={choice === 'version'} onChange={() => setChoice('version')} />
            {versionLabel}
          </label>
          {choice === 'version' && recent.length > 0 && (
            <label className="indent">
              Name the current version to keep it (optional)
              <input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Before the new upload" />
            </label>
          )}
          <label className="check">
            <input type="radio" name="conflict" checked={choice === 'keep'} onChange={() => setChoice('keep')} />
            Keep both
          </label>
          <label className="check">
            <input type="radio" name="conflict" checked={choice === 'skip'} onChange={() => setChoice('skip')} />
            Skip
          </label>
        </fieldset>
        <ErrorNote error={error} />
        <div className="actions">
          <button type="button" onClick={props.onClose}>
            Cancel
          </button>
          <button type="submit" className="primary" disabled={busy}>
            {choice === 'version' && recent.length > 0 && label.trim() ? 'Keep it and upload' : 'Continue'}
          </button>
        </div>
      </form>
    </Dialog>
  )
}
