import { Link, useSearchParams } from 'react-router'
import { client, unwrap } from '../../api/client'
import { useApi } from '../../api/useApi'
import { usePagedApi } from '../../api/usePagedApi'
import { tenantPath, useTenant } from '../../app/tenant'
import { formatDate } from '../../components/format'
import {
  Button,
  EmptyState,
  Field,
  Grow,
  List,
  ListRow,
  ListRowMain,
  Loaded,
  LoadMore,
  PageBody,
  Row,
  Select,
  Text,
  TextInput,
} from '../../ui'
import { t } from '../../i18n'

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
    <PageBody>
      <h1>{t('search.title')}</h1>
      <Row
        as="form"
        align="end"
        role="search"
        onSubmit={(e) => {
          e.preventDefault()
          update('q', String(new FormData(e.currentTarget as HTMLFormElement).get('q') ?? '').trim())
        }}
      >
        <Grow as="div">
          <Field label={t('search.name')}>
            <TextInput name="q" type="search" defaultValue={q} key={q} />
          </Field>
        </Grow>
        <Field label={t('search.tag')}>
          <Select value={tag} onChange={(e) => update('tag', e.target.value)}>
            <option value="">{t('search.anyTag')}</option>
            {tags.data?.items.map((tag) => (
              <option key={tag.id} value={tag.id}>
                {tag.name}
              </option>
            ))}
          </Select>
        </Field>
        <Button type="submit" icon="search">
          {t('search.submit')}
        </Button>
      </Row>
      {q && (
        <Loaded state={results}>
          {({ items }) =>
            items.length === 0 ? (
              <EmptyState icon="search" title={t('search.none', { q })} />
            ) : (
              <>
                <Text role="status" tone="muted">
                  {t('search.count', { n: items.length })}
                </Text>
                <List>
                  {items.map(({ node, path }) => (
                    <ListRow key={node.id}>
                      <ListRowMain>
                        <Link
                          to={
                            node.kind === 'folder'
                              ? tenantPath(tenant.id, 'files', node.id)
                              : `${tenantPath(tenant.id, 'files', node.parent_id ?? '')}?node=${node.id}`
                          }
                        >
                          {node.name}
                        </Link>
                        <Text as="span" tone="muted" size="sm">
                          {' '}
                          {path}
                        </Text>
                      </ListRowMain>
                      <Text as="span" tone="muted" size="sm" hideNarrow>
                        {formatDate(node.updated_at)}
                      </Text>
                    </ListRow>
                  ))}
                </List>
              </>
            )
          }
        </Loaded>
      )}
      {q && <LoadMore state={results} />}
    </PageBody>
  )
}
