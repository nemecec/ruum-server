import { Link } from 'react-router'

export function NotFoundPage() {
  return (
    <div className="page">
      <h1>Page not found</h1>
      <p>
        <Link to="/">Go to your files</Link>
      </p>
    </div>
  )
}
