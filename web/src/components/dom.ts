/** Selects a file name without its extension ("plan" of "plan.txt"), as file managers do. */
export function selectBaseName(input: HTMLInputElement): void {
  const dot = input.value.lastIndexOf('.')
  input.setSelectionRange(0, dot > 0 ? dot : input.value.length)
}

/**
 * Moves the focus to the page's h1 (or the main region) after the focused element went away, for
 * example a row that moved to the trash (design-system 4.4). Runs after the next paint.
 */
export function focusPageHeading() {
  requestAnimationFrame(() => {
    const active = document.activeElement
    if (active && active !== document.body && active.isConnected) return
    const main = document.getElementById('main')
    const h1 = main?.querySelector<HTMLElement>('h1') ?? main
    if (!h1) return
    if (!h1.hasAttribute('tabindex')) h1.setAttribute('tabindex', '-1')
    h1.focus()
  })
}
