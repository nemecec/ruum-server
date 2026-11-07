// The rules of the design system that stylelint cannot see (design 12.5):
//
// 1. Style files (.css, .scss, .sass, .less) live only in src/ui/ and src/design/.
// 2. No inline styles: no `style` attribute on any JSX element.
// 3. Screens hold no styling: no `className` in src/app/, src/pages/ and src/components/, and no
//    raw controls there that the library has a component for (button, input, select, textarea,
//    table and its parts, dialog, progress, meter, label, fieldset). Screens use src/ui.
// 4. Every component folder of src/ui/ has a Ladle story file (`*.stories.tsx`).
// 5. No literal UI text (design 12.4): in src/app/, src/pages/, src/components/ and src/ui/ (not
//    stories or tests), words come from the message catalogue (src/locales/en.json, `t()`). The
//    rule finds JSX text with a letter, and a string or template literal with a letter as a text
//    prop (label, title, aria-label, placeholder, ...), as a JSX child (also inside `?:`, `&&`),
//    as a text property of an object (`{ label: '...' }`) or as the text of `toast()` and
//    `setNotice()`. A string without letters (" ", "-", "/") or that is a catalogue key is fine;
//    `// i18n-ignore: <reason>` on the line before skips one literal that people never see.
//
// Run: node scripts/check-ui-rules.mjs (part of `npm run lint`). Exit code 1 lists each break.
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const web = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
const src = path.join(web, 'src')
const screenDirs = ['app', 'pages', 'components'].map((d) => path.join(src, d))
const styleDirs = ['ui', 'design'].map((d) => path.join(src, d))
const rawElements = new Set([
  'button',
  'input',
  'select',
  'textarea',
  'table',
  'thead',
  'tbody',
  'tfoot',
  'tr',
  'th',
  'td',
  'caption',
  'dialog',
  'progress',
  'meter',
  'label',
  'fieldset',
  'legend',
  'style',
])

const problems = []
const rel = (f) => path.relative(web, f)

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name)
    if (e.isDirectory()) walk(p, out)
    else out.push(p)
  }
  return out
}

const files = walk(src)
const inside = (f, dirs) => dirs.some((d) => f === d || f.startsWith(d + path.sep))

// 1. Style files.
for (const f of files) {
  if (/\.(css|scss|sass|less)$/.test(f) && !inside(f, styleDirs)) problems.push(`${rel(f)}: style files belong in src/ui/ or src/design/`)
}

// 2 and 3. JSX.
for (const f of files.filter((f) => /\.tsx$/.test(f))) {
  const text = fs.readFileSync(f, 'utf8')
  const sf = ts.createSourceFile(f, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX)
  const screen = inside(f, screenDirs)
  const at = (node) => `${rel(f)}:${sf.getLineAndCharacterOfPosition(node.getStart()).line + 1}`
  const visit = (node) => {
    if (ts.isJsxAttribute(node)) {
      const name = node.name.getText(sf)
      if (name === 'style') problems.push(`${at(node)}: no inline styles (use a component of src/ui)`)
      if (name === 'className' && screen) problems.push(`${at(node)}: no className in screens (use a component of src/ui)`)
    }
    if (screen && (ts.isJsxOpeningElement(node) || ts.isJsxSelfClosingElement(node))) {
      const tag = node.tagName.getText(sf)
      if (rawElements.has(tag)) problems.push(`${at(node)}: <${tag}> in a screen (use the component of src/ui)`)
    }
    if (screen && ts.isImportDeclaration(node) && /\.(css|scss|sass|less)['"]$/.test(node.moduleSpecifier.getText(sf)))
      problems.push(`${at(node)}: screens import no style files`)
    ts.forEachChild(node, visit)
  }
  visit(sf)
}

// 5. Literal UI text.
const textDirs = ['app', 'pages', 'components', 'ui'].map((d) => path.join(src, d))
const textProps = new Set([
  'label',
  'title',
  'aria-label',
  'aria-description',
  'aria-roledescription',
  'aria-valuetext',
  'aria-placeholder',
  'placeholder',
  'alt',
  'hint',
  'text',
  'message',
  'confirmLabel',
  'ariaLabel',
  'srText',
  'legend',
  'caption',
  'asideLabel',
  'pickLabel',
  'submitLabel',
  'help',
  'heading',
  'description',
  'note',
])
const textKeys = new Set(['label', 'title', 'text', 'message', 'hint', 'description', 'placeholder', 'help', 'heading', 'note'])
const textCalls = new Set(['toast', 'setNotice'])
const catalogueKeys = new Set(Object.keys(JSON.parse(fs.readFileSync(path.join(src, 'locales', 'en.json'), 'utf8')).messages))
// A catalogue key is not UI text.
const hasLetter = (s) => /\p{L}/u.test(s) && !catalogueKeys.has(s)

/** A literal with a letter in it, also inside `a ? b : c`, `a && b`, `(a)` and templates. */
function literalText(node) {
  if (!node) return null
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) return hasLetter(node.text) ? node : null
  if (ts.isTemplateExpression(node)) {
    const parts = [node.head.text, ...node.templateSpans.map((s) => s.literal.text)]
    if (parts.some(hasLetter)) return node
    for (const span of node.templateSpans) {
      const inner = literalText(span.expression)
      if (inner) return inner
    }
    return null
  }
  if (ts.isParenthesizedExpression(node)) return literalText(node.expression)
  if (ts.isConditionalExpression(node)) return literalText(node.whenTrue) ?? literalText(node.whenFalse)
  if (ts.isBinaryExpression(node)) {
    const op = node.operatorToken.kind
    if (op === ts.SyntaxKind.AmpersandAmpersandToken) return literalText(node.right)
    if (op === ts.SyntaxKind.BarBarToken || op === ts.SyntaxKind.QuestionQuestionToken || op === ts.SyntaxKind.PlusToken)
      return literalText(node.left) ?? literalText(node.right)
  }
  return null
}

for (const f of files.filter((f) => /\.tsx?$/.test(f) && !/\.(stories|test)\.tsx?$/.test(f) && !f.endsWith('.d.ts'))) {
  if (!inside(f, textDirs)) continue
  const text = fs.readFileSync(f, 'utf8')
  const sf = ts.createSourceFile(f, text, ts.ScriptTarget.Latest, true, f.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS)
  const at = (node) => `${rel(f)}:${sf.getLineAndCharacterOfPosition(node.getStart()).line + 1}`
  const lines = text.split('\n')
  const report = (node, what) =>
    /i18n-ignore/.test(lines[sf.getLineAndCharacterOfPosition(node.getStart()).line - 1] ?? '') ||
    problems.push(`${at(node)}: literal UI text ${JSON.stringify(node.getText(sf).trim().slice(0, 60))} ${what} (use t() and src/locales/en.json)`)
  const visit = (node) => {
    if (ts.isJsxText(node) && hasLetter(node.text)) report(node, 'in JSX')
    if (ts.isJsxAttribute(node) && node.initializer && textProps.has(node.name.getText(sf))) {
      const init = node.initializer
      const lit = ts.isStringLiteral(init) ? literalText(init) : ts.isJsxExpression(init) ? literalText(init.expression) : null
      if (lit) report(lit, `in the prop ${node.name.getText(sf)}`)
    }
    if (ts.isJsxExpression(node) && node.parent && (ts.isJsxElement(node.parent) || ts.isJsxFragment(node.parent))) {
      const lit = literalText(node.expression)
      if (lit) report(lit, 'as a JSX child')
    }
    if (ts.isPropertyAssignment(node) && textKeys.has(node.name.getText(sf).replace(/['"]/g, ''))) {
      const lit = literalText(node.initializer)
      if (lit) report(lit, `as the property ${node.name.getText(sf)}`)
    }
    if (ts.isCallExpression(node) && ts.isIdentifier(node.expression) && textCalls.has(node.expression.text)) {
      const lit = literalText(node.arguments[0])
      if (lit) report(lit, `in ${node.expression.text}()`)
    }
    ts.forEachChild(node, visit)
  }
  visit(sf)
}

// 4. Stories.
for (const e of fs.readdirSync(path.join(src, 'ui'), { withFileTypes: true })) {
  if (!e.isDirectory()) continue
  const dir = path.join(src, 'ui', e.name)
  if (!fs.readdirSync(dir).some((n) => n.endsWith('.stories.tsx')))
    problems.push(`${rel(dir)}: a component folder needs a *.stories.tsx file (Ladle)`)
}

if (problems.length) {
  console.error(problems.join('\n'))
  console.error(`\n${problems.length} design system rule(s) broken. See docs/ux/design-system.md, section 6.`)
  process.exit(1)
}
console.log('design system rules: ok')
