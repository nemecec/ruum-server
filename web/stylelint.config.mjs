// Stylelint for the design system (design 12.5, docs/ux/design-system.md section 6): component
// styles use only the tokens of src/design/tokens (CSS custom properties). No colour, length,
// font, shadow, z-index or timing value is written outside the token files. The generated token
// CSS is the only place with raw values.
const tokenOnly = [
  '/color$/',
  'fill',
  'stroke',
  'font',
  'font-family',
  'font-size',
  'font-weight',
  'line-height',
  'box-shadow',
  'z-index',
  'opacity',
  'transition',
  'transition-duration',
  'animation-duration',
  'border-radius',
]

export default {
  plugins: ['stylelint-declaration-strict-value'],
  ignoreFiles: ['src/design/generated/**', 'dist/**', 'node_modules/**'],
  rules: {
    // Colours: never a literal.
    'color-no-hex': true,
    'color-named': 'never',
    'function-disallowed-list': ['rgb', 'rgba', 'hsl', 'hsla', 'hwb', 'lab', 'lch', 'oklab', 'oklch', 'color', 'color-mix'],
    // Lengths: no unit that makes a length (or a duration). Percentages, fractions and viewport
    // units are relations, not values of the design; they are allowed. Media queries cannot read
    // custom properties, so px is allowed there only (the values of breakpoint.* tokens).
    'unit-disallowed-list': [
      ['px', 'rem', 'em', 'ch', 'ex', 'pt', 'pc', 'cm', 'mm', 'in', 'q', 'vmin', 'vmax', 'lh', 'rlh', 'ms', 's'],
      { ignoreMediaFeatureNames: { px: ['min-width', 'max-width', 'width'] } },
    ],
    // Fonts, shadows, layers, radii, timing: only through a token.
    'scale-unlimited/declaration-strict-value': [
      tokenOnly,
      {
        ignoreValues: [
          'currentColor',
          'currentcolor',
          'transparent',
          'inherit',
          'initial',
          'unset',
          'none',
          '0',
          '1',
          'normal',
          'auto',
          'linear',
          'infinite',
          '/^calc\\(/',
          '/^min\\(/',
        ],
        expandShorthand: false,
        disableFix: true,
      },
    ],
    'declaration-no-important': true,
    'selector-max-id': 0,
  },
}
