// The Vite settings of the catalogue. Ladle brings its own Vite and React plugin; the app's
// vite.config.ts (dev proxy, mock API, Vitest) is not needed here. Ladle keeps its own cache, so
// `ladle serve` and `vite --mode mock` can run side by side from web/ (UX panel, consistency 14).
export default { cacheDir: 'node_modules/.vite-ladle' }
