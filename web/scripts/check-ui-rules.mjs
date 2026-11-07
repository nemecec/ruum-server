// The rules of the design system that stylelint cannot see (design 12.5):
//
// 1. Style files (.css, .scss, .sass, .less) live only in src/ui/ and src/design/.
// 2. No inline styles: no `style` attribute on any JSX element.
// 3. Screens hold no styling: no `className` in src/app/, src/pages/ and src/components/, and no
//    raw controls there that the library has a component for (button, input, select, textarea,
//    table and its parts, dialog, progress, meter, label, fieldset). Screens use src/ui.
// 4. Every component folder of src/ui/ has a Ladle story file (`*.stories.tsx`).
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
