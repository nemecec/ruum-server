// Browser actions that leave the single-page app. They sit on one object, so tests can replace them
// with `vi.spyOn(browser, 'assign')`.
export const browser = {
  /** Full page navigation, for example to the identity provider or back to an OAuth client. */
  assign(url: string): void {
    window.location.assign(url)
  },
  /** Starts a download of `url` (a short-lived content-domain URL from `getContentUrl`). */
  download(url: string, filename: string): void {
    const a = document.createElement('a')
    a.href = url
    a.download = filename
    a.rel = 'noopener'
    document.body.append(a)
    a.click()
    a.remove()
  },
  /** Opens `url` in a new tab (inline previews on the content domain). */
  open(url: string): void {
    window.open(url, '_blank', 'noopener')
  },
}
