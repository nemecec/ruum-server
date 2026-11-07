import { useSyncExternalStore } from 'react'
import { Banner } from '../ui'
import { t } from '../i18n'

function subscribe(onChange: () => void) {
  window.addEventListener('online', onChange)
  window.addEventListener('offline', onChange)
  return () => {
    window.removeEventListener('online', onChange)
    window.removeEventListener('offline', onChange)
  }
}

/** The offline banner (journeys "Common states", copy.md section 4): the UI tells the truth. */
export function OfflineBanner() {
  const online = useSyncExternalStore(
    subscribe,
    () => navigator.onLine,
    () => true,
  )
  if (online) return null
  return <Banner role="alert">{t('shell.offline')}</Banner>
}
