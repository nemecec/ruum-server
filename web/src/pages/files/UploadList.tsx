import { errorText } from '../../api/client'
import { formatBytes } from '../../components/format'
import type { UploadProgress } from '../../upload/upload'
import type { UploadItem } from './useUploads'

const phaseLabels: Record<UploadProgress['phase'], string> = {
  hashing: 'Checking',
  uploading: 'Uploading',
  completing: 'Finishing',
}

function statusText(item: UploadItem): string {
  switch (item.status) {
    case 'waiting':
      return 'Waiting'
    case 'done':
      return 'Uploaded'
    case 'canceled':
      return 'Canceled'
    case 'failed':
      return `Failed: ${errorText(item.error)}`
    case 'active': {
      const { phase, done, total } = item.progress
      const pct = total ? Math.floor((done / total) * 100) : 0
      return `${phaseLabels[phase]} ${pct}%`
    }
  }
}

/** The list of uploads with their progress and a cancel button. */
export function UploadList(props: { items: UploadItem[]; onCancel: (id: string) => void; onClear: () => void }) {
  const { items } = props
  if (items.length === 0) return null
  const finished = items.some((i) => i.status !== 'active' && i.status !== 'waiting')
  return (
    <section className="uploads" aria-label="Uploads">
      <ul>
        {items.map((i) => {
          const { done, total } = i.progress
          return (
            <li key={i.id} className={`upload upload-${i.status}`}>
              <div className="upload-head">
                <span className="upload-name">{i.name}</span>
                <span className="muted small">{formatBytes(i.size)}</span>
              </div>
              {i.status === 'active' && (
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
    </section>
  )
}
