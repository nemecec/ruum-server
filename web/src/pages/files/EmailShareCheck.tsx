import { useEffect, useState } from 'react'
import { Link } from 'react-router'
import { client, unwrap } from '../../api/client'
import { browser } from '../../app/browser'
import { tenantPath } from '../../app/tenant'
import { useSession } from '../../app/useSession'
import { Actions, Button, ButtonLink, EmptyState, ErrorNote, Loading, PageBody, Text } from '../../ui'
import { t } from '../../i18n'

// The items for which this page load already asked the server to check email grants. The check
// runs once per item: a second `404` means the share is not for this account.
const checked = new Set<string>()

/** Forgets the checks (tests). */
export function resetEmailShareChecks() {
  checked.clear()
}

/**
 * The item of a share email answered `404` (decision 0003-2, 0003-3). A share to the person's
 * email binds to their account only when they use Ruum with the session cookie; the server
 * throttles that check, so the page asks for it once (`listSharedWithMe` with
 * `check_email_grants=true`), reloads the spaces (the bind can make the person a guest of a new
 * space) and tries the item again (`onRetry`). Still `404`: the share is for another address, or
 * the account's email is not verified yet. The page never names the sharer or the item.
 */
export function EmailShareCheck(props: { nodeId: string; tenantId: string; onRetry: () => void; loading?: boolean }) {
  const { nodeId, tenantId, onRetry } = props
  const { me, reloadTenants } = useSession()
  const key = `${tenantId}/${nodeId}`
  const [state, setState] = useState<'checking' | 'done'>(checked.has(key) ? 'done' : 'checking')
  const [error, setError] = useState<unknown>()
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (state !== 'checking') return
    let active = true
    checked.add(key)
    void (async () => {
      try {
        await unwrap(client.GET('/v1/shared-with-me', { params: { query: { check_email_grants: true, limit: 1 } } }))
        await reloadTenants()
      } catch {
        // The check is a help, not a step that must work: the page then says what it can.
      }
      if (!active) return
      setState('done')
      onRetry()
    })()
    return () => {
      active = false
    }
  }, [state, key, reloadTenants, onRetry])

  async function signInAgain() {
    setBusy(true)
    setError(undefined)
    try {
      const res = await unwrap(client.POST('/v1/auth/logout'))
      browser.assign(res.logout_url)
    } catch (e) {
      setError(e)
      setBusy(false)
    }
  }

  function checkAgain() {
    checked.delete(key)
    setState('checking')
  }

  if (state === 'checking' || props.loading) return <Loading label={t('item.check.loading')} />

  if (!me.user.email_verified) {
    return (
      <PageBody narrow>
        <EmptyState icon="info" title={t('item.unverified.title')} text={t('item.unverified.text')}>
          <Button variant="primary" onClick={checkAgain}>
            {t('item.unverified.again')}
          </Button>
        </EmptyState>
      </PageBody>
    )
  }

  return (
    <PageBody narrow>
      <EmptyState icon="lock" title={t('item.otherAddress.title')} text={t('item.otherAddress.text')}>
        <Actions align="center">
          <Button variant="primary" busy={busy} disabled={busy} onClick={signInAgain}>
            {t('item.otherAddress.signIn')}
          </Button>
          <ButtonLink to={tenantPath(me.user.home_tenant_id, 'shared')}>{t('shell.nav.shared')}</ButtonLink>
        </Actions>
      </EmptyState>
      <Text tone="muted" size="sm">
        {t('item.otherAddress.new')}
      </Text>
      <ErrorNote error={error} />
      <Text tone="muted" size="sm">
        <Link to="/">{t('common.goToFiles')}</Link>
      </Text>
    </PageBody>
  )
}
