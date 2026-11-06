/** Selects a file name without its extension ("plan" of "plan.txt"), as file managers do. */
export function selectBaseName(input: HTMLInputElement): void {
  const dot = input.value.lastIndexOf('.')
  input.setSelectionRange(0, dot > 0 ? dot : input.value.length)
}
