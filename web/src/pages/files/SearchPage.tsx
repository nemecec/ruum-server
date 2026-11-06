import { Link, useSearchParams } from 'react-router'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDate } from '../../components/format'
import { Loaded, LoadMore } from '../../components/ui'

/** Search by name, optionally narrowed to one tag. */
export function SearchPage() {
  const { tenant } = useTenant()
  const [params, setParams] = useSearchParams()
  const q = params.get('q')?.trim() ?? ''
  const tag = params.get('tag') ?? ''
  const tags = useApi(() => unwrap(client.GET('/v1/tags')), [])
  const results = usePagedApi(
    (cursor) =>
      q
        ? unwrap(client.GET('/v1/search', { params: { query: { q, ...(tag ? { tag } : {}), ...(cursor ? { cursor } : {}) } } }))
        : Promise.resolve({ items: [], next_cursor: null }),
    [q, tag],
  )

  function update(name: string, value: string) {
    const next = new URLSearchParams(params)
    if (value) next.set(name, value)
    else next.delete(name)
    setParams(next)
  }

  return (
    <div className="page-body">
      <h1>Search</h1>
      <form
        className="inline-form"
        role="search"
        onSubmit={(e) => {
          e.preventDefault()
          update('q', String(new FormData(e.currentTarget).get('q') ?? '').trim())
        }}
      >
        <label className="grow">
          Name contains
          <input name="q" type="search" defaultValue={q} key={q} />
        </label>
        <label>
          Tag
          <select value={tag} onChange={(e) => update('tag', e.target.value)}>
            <option value="">Any tag</option>
            {tags.data?.items.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
        </label>
        <button type="submit">Search</button>
      </form>
      {q && (
        <Loaded state={results}>
          {({ items }) =>
            items.length === 0 ? (
              <p className="empty">Nothing matches "{q}".</p>
            ) : (
              <>
                <p role="status" className="muted">
                  {items.length} {items.length === 1 ? 'match' : 'matches'}
                </p>
                <ul className="rows">
                  {items.map(({ node, path }) => (
                    <li key={node.id} className="row">
                      <span className="grow">
                        <Link
                          to={
                            node.kind === 'folder'
                              ? tenantPath(tenant.id, 'files', node.id)
                              : `${tenantPath(tenant.id, 'files', node.parent_id ?? '')}?node=${node.id}`
                          }
                        >
                          {node.name}
                        </Link>
                        <span className="muted small"> {path}</span>
                      </span>
                      <span className="muted small hide-narrow">{formatDate(node.updated_at)}</span>
                    </li>
                  ))}
                </ul>
              </>
            )
          }
        </Loaded>
      )}
      {q && <LoadMore state={results} />}
    </div>
  )
}
