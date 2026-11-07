/** Joins class names, skipping empty ones. Only for components of src/ui. */
export function cx(...names: (string | false | null | undefined)[]): string {
  return names.filter(Boolean).join(' ')
}
