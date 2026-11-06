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
  /**
   * Opens an empty tab, to point it at a URL that is not known yet (`navigateTab`). Without
   * `noopener` here, so the page can set its address; `navigateTab` cuts the link back to the page.
   */
  openBlank(): Window | null {
    return window.open('about:blank', '_blank')
  },
  /** Points a tab from `openBlank` at `url`, or opens a new tab when the browser blocked it. */
  navigateTab(tab: Window | null, url: string): void {
    if (!tab || tab.closed) {
      window.open(url, '_blank', 'noopener')
      return
    }
    tab.opener = null
    tab.location.href = url
  },
}
