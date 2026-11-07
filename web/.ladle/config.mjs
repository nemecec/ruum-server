// Ladle: the component catalogue of src/ui (design 12.5). `npm run ladle` serves it, `npm run
// ladle:build` builds it to build-ladle/. The theme button (or the key "d") switches light and
// dark: Ladle sets data-theme on <html>, as the Account page does. The a11y panel runs axe.
/** @type {import('@ladle/react').UserConfig} */
export default {
  stories: 'src/ui/**/*.stories.tsx',
  viteConfig: '.ladle/vite.config.mjs',
  outDir: 'build-ladle',
  addons: {
    a11y: { enabled: true },
    theme: { enabled: true, defaultState: 'light' },
    rtl: { enabled: true, defaultState: false },
    width: {
      enabled: true,
      options: { phone: 390, desktop: 1280 },
      defaultState: 0,
    },
  },
}
