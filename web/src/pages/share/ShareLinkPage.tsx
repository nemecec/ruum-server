import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react'
import { useLocation } from 'react-router'
import { client, linkClient, RuumApiError, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { browser } from '../../app/browser'
import { fileTypeLabel, formatBytes, formatDate, formatWhen, roleLabels } from '../../components/format'
import { EmptyState, ErrorNote, FileIcon, FolderIcon, Loaded, Loading } from '../../components/ui'

type Node = Schemas['Node']
type Resolved = Schemas['ResolvedShareLink']

type State =
  | { step: 'resolving' }
  | { step: 'password'; wrong: boolean }
  | { step: 'ended' }
  | { step: 'failed'; error: unknown }
  | { step: 'ready'; link: Resolved }


/** Opens a link: `resolveShareLink` with the token (and password) in the body. */
async function resolveLink(token: string, password?: string): Promise<State> {
  try {
    const link = await unwrap(client.POST('/v1/share-links/resolve', { body: { token, ...(password ? { password } : {}) } }))
    return { step: 'ready', link }
  } catch (e) {
    if (e instanceof RuumApiError && e.status === 401) return { step: 'password', wrong: Boolean(password) }
    if (e instanceof RuumApiError && e.status === 404) return { step: 'ended' }
    return { step: 'failed', error: e }
  }
}

/**
 * `/s#<token>`: a visitor opens a share link without an account (journeys J3 step 8, card UX-01).
 * The page sends the token in the body of `resolveShareLink`, asks for the password when the link
 * has one, and shows the shared item with the reduced shell. The access token stays in memory.
 */
export function ShareLinkPage() {
  // The token is the part after `#`: it never goes into a request URL or a log.
  const { hash } = useLocation()
  const token = decodeURIComponent(hash.replace(/^#/, ''))
  const [state, setState] = useState<State>(token ? { step: 'resolving' } : { step: 'ended' })

  const resolve = useCallback(
    (password?: string) => {
      setState({ step: 'resolving' })
      void resolveLink(token, password).then(setState)
    },
    [token],
  )

  useEffect(() => {
    if (token) void resolveLink(token).then(setState)
  }, [token])

  return (
    <>
      <header className="topbar">
        <div className="topbar-row">
          <span className="brand">Ruum</span>
          {state.step === 'ready' && (
            <span className="muted small">
              You're viewing as anyone with the link. You can {roleLabels[state.link.role].replace('Can ', '')}.
            </span>
          )}
        </div>
      </header>
      <main id="main" className="main">
        {state.step === 'resolving' && <Loading label="Opening the link" />}
        {state.step === 'ended' && (
          <div className="page narrow">
            <h1>This link doesn't work any more</h1>
            <p>Ask the person who sent it for a new one.</p>
          </div>
        )}
        {state.step === 'failed' && (
          <div className="page narrow">
            <h1>Ruum couldn't open this link</h1>
            <ErrorNote error={state.error} />
            <button
              type="button"
              onClick={() => resolve()}
            >
              Try again
            </button>
          </div>
        )}
        {state.step === 'password' && (
          <PasswordForm
            wrong={state.wrong}
            onSubmit={(p) => resolve(p)}
          />
        )}
        {state.step === 'ready' && (
          <SharedView
            link={state.link}
            onExpired={() => resolve()}
          />
        )}
      </main>
    </>
  )
}

function PasswordForm({ wrong, onSubmit }: { wrong: boolean; onSubmit: (password: string) => void }) {
  const [password, setPassword] = useState('')
  function submit(e: FormEvent) {
    e.preventDefault()
    onSubmit(password)
  }
  return (
    <form className="page narrow stack" onSubmit={submit}>
      <h1>This link has a password</h1>
      <label>
        Password
        <input type="password" required autoFocus autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
      </label>
      {wrong && (
        <p className="error" role="alert">
          That password isn't right. Try again.
        </p>
      )}
      <div className="actions left">
        <button type="submit" className="primary">
          Open
        </button>
      </div>
    </form>
  )
}

/** The shared item: a folder with its subfolders and files, or one file. */
function SharedView({ link, onExpired }: { link: Resolved; onExpired: () => void }) {
  const api = useMemo(() => linkClient(link.access_token, link.tenant_id), [link.access_token, link.tenant_id])
  const [trail, setTrail] = useState<Node[]>([link.node])
  const [error, setError] = useState<unknown>()
  const current = trail[trail.length - 1]
  const children = useApi(
    async () => {
      if (current.kind !== 'folder') return []
      const all: Node[] = []
      let cursor: string | undefined
      for (let i = 0; i < 20; i++) {
        const page = await unwrap(
          api.GET('/v1/nodes/{node_id}/children', { params: { path: { node_id: current.id }, query: cursor ? { cursor } : {} } }),
        )
        all.push(...page.items)
        if (!page.next_cursor) break
        cursor = page.next_cursor
      }
      return all
    },
    [current.id, api],
  )

  useEffect(() => {
    if (children.error instanceof RuumApiError && children.error.status === 401) onExpired()
  }, [children.error, onExpired])

  async function open(node: Node, disposition: 'attachment' | 'inline') {
    setError(undefined)
    const tab = disposition === 'inline' ? browser.openBlank() : null
    try {
      const res = await unwrap(
        api.GET('/v1/nodes/{node_id}/content-url', { params: { path: { node_id: node.id }, query: { disposition } } }),
      )
      if (disposition === 'inline') browser.navigateTab(tab, res.url)
      else browser.download(res.url, node.name)
    } catch (e) {
      tab?.close()
      setError(e)
    }
  }

  if (current.kind === 'file') {
    return (
      <div className="page narrow stack">
        <h1>{current.name}</h1>
        <dl className="facts">
          <dt>Type</dt>
          <dd>{fileTypeLabel(current)}</dd>
          <dt>Size</dt>
          <dd>{formatBytes(current.size_bytes)}</dd>
          <dt>Changed</dt>
          <dd title={formatDate(current.updated_at)}>{formatWhen(current.updated_at)}</dd>
        </dl>
        <ErrorNote error={error} />
        <div className="actions left">
          <button type="button" className="primary" onClick={() => open(current, 'attachment')}>
            Download
          </button>
          <button type="button" onClick={() => open(current, 'inline')}>
            Preview
          </button>
        </div>
      </div>
    )
  }

  return (
    <div className="page-body">
      <nav aria-label="Breadcrumbs" className="breadcrumbs" hidden={trail.length < 2}>
        <ol>
          {trail.map((n, i) => (
            <li key={n.id}>
              {i === trail.length - 1 ? (
                <span aria-current="page">{n.name}</span>
              ) : (
                <button type="button" className="link" onClick={() => setTrail(trail.slice(0, i + 1))}>
                  {n.name}
                </button>
              )}
            </li>
          ))}
        </ol>
      </nav>
      <h1>{current.name}</h1>
      <ErrorNote error={error} />
      <Loaded state={children}>
        {(items) =>
          items.length === 0 ? (
            <EmptyState title="This folder is empty" />
          ) : (
            <table className="table files">
              <caption className="sr-only">Contents of this folder</caption>
              <thead>
                <tr>
                  <th scope="col">Name</th>
                  <th scope="col" className="hide-narrow">
                    Size
                  </th>
                  <th scope="col" className="hide-narrow">
                    Changed
                  </th>
                  <th scope="col">
                    <span className="sr-only">Actions</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {items.map((n) => (
                  <tr key={n.id}>
                    <th scope="row">
                      <span className="name-cell">
                        <span className={`kind kind-${n.kind}`}>{n.kind === 'folder' ? <FolderIcon /> : <FileIcon type={n.type} name={n.name} />}</span>
                        <button type="button" className="link" onClick={() => setTrail([...trail, n])}>
                          {n.name}
                        </button>
                      </span>
                    </th>
                    <td className="hide-narrow num">{n.kind === 'folder' ? '-' : formatBytes(n.size_bytes)}</td>
                    <td className="hide-narrow">
                      <span title={formatDate(n.updated_at)}>{formatWhen(n.updated_at)}</span>
                    </td>
                    <td className="row-actions">
                      {n.kind === 'file' && (
                        <button type="button" aria-label={`Download ${n.name}`} onClick={() => open(n, 'attachment')}>
                          Download
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )
        }
      </Loaded>
    </div>
  )
}
