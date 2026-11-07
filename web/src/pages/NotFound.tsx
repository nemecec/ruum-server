import { Link } from 'react-router'
import { Brand, EmptyState, Page, PageBody } from '../ui'
import { t } from '../i18n'

/**
 * A page that does not exist (journeys, common states: "Not found"). Inside a space it sits in the
 * shell's main region; outside it is its own centred page with the brand and a `main` landmark.
 */
export function NotFoundPage({ inShell }: { inShell?: boolean }) {
  const body = (
    <EmptyState icon="info" title={t('notFound.text')}>
      <Link to="/">{t('common.goToFiles')}</Link>
    </EmptyState>
  )
  if (inShell)
    return (
      <PageBody>
        <h1>{t('notFound.title')}</h1>
        {body}
      </PageBody>
    )
  return (
    <Page id="main" tabIndex={-1} narrow>
      <Brand>{t('app.name')}</Brand>
      <h1>{t('notFound.title')}</h1>
      {body}
    </Page>
  )
}
