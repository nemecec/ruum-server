import { Link } from 'react-router'
import { Page } from '../ui'

export function NotFoundPage() {
  return (
    <Page as="div">
      <h1>Page not found</h1>
      <p>
        <Link to="/">Go to your files</Link>
      </p>
    </Page>
  )
}
