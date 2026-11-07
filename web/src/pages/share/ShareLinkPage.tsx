import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react'
import { useLocation } from 'react-router'
import { client, linkClient, RuumApiError, unwrap, type Schemas } from '../../api/client'
import { useApi } from '../../api/useApi'
import { browser } from '../../app/browser'
import { fileTypeLabel, formatBytes, formatDate, formatWhen } from '../../components/format'
import {
  TopBar,
  Table,
  NameCell,
  KindIcon,
  CurrentCrumb,
  Crumb,
  Breadcrumbs,
  Brand,
  AppMain,
  Actions,
  Button,
  EmptyState,
  ErrorNote,
  Facts,
  Field,
  Loaded,
  Loading,
  Page,
  PageBody,
  TBody,
  Td,
  Text,
  TextInput,
  Th,
  THead,
  Tr,
  VisuallyHidden,
} from '../../ui'
import { t } from '../../i18n'

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
      <TopBar>
        <Brand>{t('app.name')}</Brand>
        {state.step === 'ready' && (
          <Text as="span" tone="muted" size="sm">
            {t('link.viewing', { role: state.link.role })}
          </Text>
        )}
      </TopBar>
      <AppMain>
        {state.step === 'resolving' && <Loading label={t('link.opening')} />}
        {state.step === 'ended' && (
          <Page narrow as="div">
            <h1>{t('link.ended.title')}</h1>
            <p>{t('link.ended.text')}</p>
          </Page>
        )}
        {state.step === 'failed' && (
          <Page narrow as="div">
            <h1>{t('link.failed.title')}</h1>
            <ErrorNote error={state.error} />
            <Button onClick={() => resolve()}>{t('common.tryAgain')}</Button>
          </Page>
        )}
        {state.step === 'password' && <PasswordForm wrong={state.wrong} onSubmit={(p) => resolve(p)} />}
        {state.step === 'ready' && <SharedView link={state.link} onExpired={() => resolve()} />}
      </AppMain>
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
    <Page narrow as="form" onSubmit={submit}>
      <h1>{t('link.password.title')}</h1>
      <Field label={t('link.password')}>
        <TextInput
          type="password"
          required
          autoFocus
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </Field>
      {wrong && (
        <Text tone="danger" role="alert">
          {t('link.password.wrong')}
        </Text>
      )}
      <Actions align="start">
        <Button variant="primary" type="submit">
          {t('link.open')}
        </Button>
      </Actions>
    </Page>
  )
}

/** The shared item: a folder with its subfolders and files, or one file. */
function SharedView({ link, onExpired }: { link: Resolved; onExpired: () => void }) {
  const api = useMemo(() => linkClient(link.access_token, link.tenant_id), [link.access_token, link.tenant_id])
  const [trail, setTrail] = useState<Node[]>([link.node])
  const [error, setError] = useState<unknown>()
  const current = trail[trail.length - 1]
  const children = useApi(async () => {
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
  }, [current.id, api])

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
      <Page narrow as="div">
        <h1>{current.name}</h1>
        <Facts>
          <dt>{t('details.type')}</dt>
          <dd>{fileTypeLabel(current)}</dd>
          <dt>{t('files.size')}</dt>
          <dd>{formatBytes(current.size_bytes)}</dd>
          <dt>{t('files.changed')}</dt>
          <dd title={formatDate(current.updated_at)}>{formatWhen(current.updated_at)}</dd>
        </Facts>
        <ErrorNote error={error} />
        <Actions align="start">
          <Button variant="primary" onClick={() => open(current, 'attachment')}>
            {t('common.download')}
          </Button>
          <Button onClick={() => open(current, 'inline')}>{t('details.preview')}</Button>
        </Actions>
      </Page>
    )
  }

  return (
    <PageBody>
      <Breadcrumbs hidden={trail.length < 2}>
        {trail.map((n, i) =>
          i === trail.length - 1 ? (
            <CurrentCrumb key={n.id}>{n.name}</CurrentCrumb>
          ) : (
            <Crumb key={n.id}>
              <Button variant="link" onClick={() => setTrail(trail.slice(0, i + 1))}>
                {n.name}
              </Button>
            </Crumb>
          ),
        )}
      </Breadcrumbs>
      <h1>{current.name}</h1>
      <ErrorNote error={error} />
      <Loaded state={children}>
        {(items) =>
          items.length === 0 ? (
            <EmptyState title={t('files.empty.title')} />
          ) : (
            <Table caption={t('files.table.caption')}>
              <THead>
                <Tr>
                  <Th scope="col">{t('common.name')}</Th>
                  <Th hideNarrow scope="col">
                    {t('files.size')}
                  </Th>
                  <Th hideNarrow scope="col">
                    {t('files.changed')}
                  </Th>
                  <Th scope="col">
                    <VisuallyHidden>{t('common.actions')}</VisuallyHidden>
                  </Th>
                </Tr>
              </THead>
              <TBody>
                {items.map((n) => (
                  <Tr key={n.id}>
                    <Th scope="row">
                      <NameCell icon={<KindIcon kind={n.kind} type={n.type} name={n.name} />}>
                        <Button variant="link" onClick={() => setTrail([...trail, n])}>
                          {n.name}
                        </Button>
                      </NameCell>
                    </Th>
                    <Td hideNarrow numeric>
                      {n.kind === 'folder' ? '-' : formatBytes(n.size_bytes)}
                    </Td>
                    <Td hideNarrow>
                      <span title={formatDate(n.updated_at)}>{formatWhen(n.updated_at)}</span>
                    </Td>
                    <Td actions>
                      {n.kind === 'file' && (
                        <Button aria-label={t('common.download.named', { name: n.name })} onClick={() => open(n, 'attachment')}>
                          {t('common.download')}
                        </Button>
                      )}
                    </Td>
                  </Tr>
                ))}
              </TBody>
            </Table>
          )
        }
      </Loaded>
    </PageBody>
  )
}
