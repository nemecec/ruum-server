import { Link } from 'react-router'
import { Page } from '../ui'
import { t } from '../i18n'

export function NotFoundPage() {
  return (
    <Page as="div">
      <h1>{t('notFound.title')}</h1>
      <p>
        <Link to="/">{t('common.goToFiles')}</Link>
      </p>
    </Page>
  )
}
