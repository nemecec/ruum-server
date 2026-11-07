// The upload queue of the whole session (design-system 4.5): it lives above the pages, so uploads
// go on and stay visible when the person opens Agents, Trash or another folder. The panel sits at
// the bottom right.

import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { errorText } from '../../api/errors'
import { formatBytes, formatWhen } from '../../components/format'
import {
  Actions,
  Button,
  Dialog,
  ErrorNote,
  Field,
  Fieldset,
  FloatingPanel,
  IconButton,
  Indent,
  List,
  ListRow,
  Note,
  ProgressBar,
  ProseList,
  Radio,
  Row,
  Stack,
  Text,
  TextInput,
  useToast,
} from '../../ui'
import { uploadBlob, type UploadProgress } from '../../upload/upload'
import type { Node } from './api'
import { UploadsContext, type DoneListener, type UploadItem, type UploadPlan } from './useUploads'
import { currentIntl, t, type MessageKey } from '../../i18n'

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
    controllers.current.get(id)?.abort(new DOMException(t('uploads.cancelled.reason'), 'AbortError'))
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
  if (n === 0) text = t('uploads.toast.failed', { n: r.failed })
  else if (n === 1 && !r.failed) text = t('uploads.toast.one', { name: r.nodes[0].name })
  else if (r.failed) text = t('uploads.toast.someFailed', { n, failed: r.failed })
  else text = folder ? t('uploads.toast.toFolder', { n, folder }) : t('uploads.toast.many', { n })
  if (r.replaced.length === 1) {
    const it = r.replaced[0]
    text += ` ${t('uploads.toast.replaced.one', { when: formatWhen(it.replaced!).toLowerCase() })}`
  } else if (r.replaced.length > 1) {
    text += ` ${t('uploads.toast.replaced', { n: r.replaced.length })}`
  }
  return text
}

const phaseLabels: Record<UploadProgress['phase'], MessageKey> = {
  hashing: 'uploads.phase.hashing',
  uploading: 'uploads.phase.uploading',
  completing: 'uploads.phase.completing',
}

function statusText(item: UploadItem): string {
  switch (item.status) {
    case 'waiting':
      return t('uploads.status.waiting')
    case 'done': {
      if (item.replaced) return t('uploads.status.joined')
      if (item.node && item.node.name !== item.name) return t('uploads.status.savedAs', { name: item.node.name })
      return t('uploads.status.done')
    }
    case 'canceled':
      return t('uploads.status.cancelled')
    case 'failed':
      return t('uploads.status.failed', { error: errorText(item.error, { name: item.name }) })
    case 'active': {
      const { phase, done, total } = item.progress
      if (phase !== 'uploading') return t(phaseLabels[phase])
      const pct = total ? Math.floor((done / total) * 100) : 0
      return t('uploads.status.progress', { phase: t(phaseLabels[phase]), pct: currentIntl().formatNumber(pct / 100, { style: 'percent' }) })
    }
  }
}

function header(items: UploadItem[]): string {
  const open = items.filter((i) => i.status === 'active' || i.status === 'waiting').length
  const failed = items.filter((i) => i.status === 'failed').length
  if (open) return t('uploads.header.running', { n: items.length - open + 1, total: items.length })
  if (failed) return t('uploads.header.failed', { n: failed })
  return t('uploads.header.done', { n: items.length })
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
    const timer = setTimeout(() => setFoldedFor(doneKey), 4000)
    return () => clearTimeout(timer)
  }, [doneKey])
  const open = manual && manual.key === doneKey ? manual.open : !(doneKey && foldedFor === doneKey)
  if (items.length === 0) return null
  const finished = items.some((i) => i.status !== 'active' && i.status !== 'waiting')
  return (
    <FloatingPanel
      label={t('uploads.label')}
      title={header(items)}
      open={open}
      actions={
        <IconButton
          icon={open ? 'minus' : 'plus'}
          aria-expanded={open}
          label={open ? t('uploads.hide') : t('uploads.show')}
          onClick={() => setManual({ key: doneKey, open: !open })}
        />
      }
    >
      <List>
        {items.map((i) => {
          const { done, total } = i.progress
          const saved = i.node && i.node.name !== i.name ? i.node.name : i.name
          return (
            <ListRow key={i.id} block>
              <Row justify="between" nowrap>
                <Text as="span" truncate>
                  {saved}
                </Text>
                <Text as="span" tone="muted" size="sm" numeric>
                  {formatBytes(i.size)}
                </Text>
              </Row>
              {i.status === 'active' && i.progress.phase === 'uploading' && (
                <ProgressBar max={total || 1} value={done} label={t('uploads.progress.label', { name: i.name })} />
              )}
              <Row justify="between" nowrap>
                <Text
                  as="span"
                  tone={i.status === 'failed' ? 'danger' : 'muted'}
                  size="sm"
                  role={i.status === 'failed' ? 'alert' : undefined}
                >
                  {statusText(i)}
                </Text>
                {(i.status === 'active' || i.status === 'waiting') && (
                  <Button size="sm" aria-label={t('uploads.cancel.label', { name: i.name })} onClick={() => props.onCancel(i.id)}>
                    {t('common.cancel')}
                  </Button>
                )}
              </Row>
            </ListRow>
          )
        })}
      </List>
      {finished && (
        <Actions align="start">
          <Button variant="link" onClick={props.onClear}>
            {t('uploads.clear')}
          </Button>
        </Actions>
      )}
    </FloatingPanel>
  )
}

export type ConflictChoice = 'version' | 'keep' | 'skip'

/** "Before the upload of 15:13": the default name that keeps a recent version (24-hour clock, local time). */
function keepLabel(now = new Date()): string {
  return t('uploads.keepLabel', { time: currentIntl().formatTime(now, { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' }) })
}

/**
 * Asks once per batch what to do with files whose names are already in the folder (copy.md 3.2):
 * save as a new version (the default), keep both, or skip. `recent` are the items whose head
 * version is the person's own unnamed save of the last 10 minutes: the server joins a new version
 * into such a version (design 5.3). So that "every change is kept as a version" holds, the
 * default names that version first (named versions never join) and says so; clearing the name
 * says in plain words that the upload then replaces it (decisions.md, synthetic-user round).
 */
export function ConflictDialog(props: {
  names: string[]
  recent?: Node[]
  onChoose: (choice: ConflictChoice, label: string) => void | Promise<void>
  onClose: () => void
}) {
  const { names } = props
  const recent = props.recent ?? []
  const [choice, setChoice] = useState<ConflictChoice>('version')
  const [label, setLabel] = useState(() => (recent.length ? keepLabel() : ''))
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)
  const title = names.length === 1 ? t('conflict.title.one', { name: names[0] }) : t('conflict.title', { n: names.length })
  const one = names.length === 1
  const when = recent.length === 1 ? formatWhen(recent[0].updated_at).toLowerCase() : null
  return (
    <Dialog title={title} open onClose={props.onClose}>
      <Stack
        as="form"
        gap={4}
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
          <ProseList size="sm">
            {names.slice(0, 8).map((n) => (
              <li key={n}>{n}</li>
            ))}
            {names.length > 8 && <li>{t('conflict.more', { n: names.length - 8 })}</li>}
          </ProseList>
        )}
        <Fieldset legend={one ? t('conflict.question.one') : t('conflict.question', { n: names.length })}>
          <Radio
            name="conflict"
            checked={choice === 'version'}
            onChange={() => setChoice('version')}
            aria-describedby="conflict-version"
            label={t('conflict.version')}
          />
          {choice === 'version' && (
            <Indent>
              <Text id="conflict-version" tone="muted" size="sm">
                {one ? t('conflict.version.help.one') : t('conflict.version.help')}
              </Text>
              {recent.length > 0 && (
                <>
                  <Field label={recent.length === 1 ? t('conflict.keepName.one') : t('conflict.keepName')}>
                    <TextInput value={label} onChange={(e) => setLabel(e.target.value)} aria-describedby="conflict-keep" />
                  </Field>
                  {label.trim() ? (
                    <Text id="conflict-keep" tone="muted" size="sm">
                      {recent.length === 1
                        ? t('conflict.kept.one', { name: recent[0].name, when: when ?? '' })
                        : t('conflict.kept', { n: recent.length })}
                    </Text>
                  ) : (
                    <Note id="conflict-keep" tone="warning" role="note">
                      {recent.length === 1 ? t('conflict.replaces.one', { when: when ?? '' }) : t('conflict.replaces')}
                    </Note>
                  )}
                </>
              )}
            </Indent>
          )}
          <Radio
            name="conflict"
            checked={choice === 'keep'}
            onChange={() => setChoice('keep')}
            label={one ? t('conflict.keep.one') : t('conflict.keep')}
          />
          <Radio name="conflict" checked={choice === 'skip'} onChange={() => setChoice('skip')} label={t('conflict.skip')} />
        </Fieldset>
        <ErrorNote error={error} />
        <Actions>
          <Button onClick={props.onClose}>{t('common.cancel')}</Button>
          <Button type="submit" variant="primary" disabled={busy} busy={busy}>
            {t('common.continue')}
          </Button>
        </Actions>
      </Stack>
    </Dialog>
  )
}
