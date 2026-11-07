import type { Story } from '@ladle/react'
import { Stack } from '../layout/Layout'
import { Code, ProseList, Text, VisuallyHidden, type Tone } from './Text'
import { sample } from '../story'

export default { title: 'Text' }

const tones: Tone[] = ['default', 'muted', 'faint', 'primary', 'success', 'warning', 'danger', 'agent']

export const Headings: Story = () => (
  <Stack>
    <h1>{sample('Page title (h1)')}</h1>
    <h2>{sample('Panel and dialog title (h2)')}</h2>
    <h3>{sample('Section title (h3)')}</h3>
    <Text>{sample('Body text in the base size. Ruum keeps people\'s work and lets agents touch it.')}</Text>
  </Stack>
)

export const Tones: Story = () => (
  <Stack gap={2}>
    {tones.map((t) => (
      <Text key={t} tone={t}>
        {t}{sample(': The quick brown fox, 12 files, 2 hours ago')}
      </Text>
    ))}
  </Stack>
)

export const Sizes: Story = () => (
  <Stack gap={2}>
    {(['xs', 'sm', 'md', 'lg'] as const).map((s) => (
      <Text key={s} size={s}>
        {sample('Size')} {s}
      </Text>
    ))}
    <Text weight="strong">{sample('Strong')}</Text>
    <Text mono>{sample('Mono: key prefix 8f3a91')}</Text>
    <Text italic tone="faint">
      {sample('NULL')}
    </Text>
    <Text numeric>1,234.50 · 98.10</Text>
  </Stack>
)

export const Truncated: Story = () => (
  <Stack>
    <Text truncate>{sample('A very long file name that does not fit on one line and is cut with an ellipsis at the end.pdf')}</Text>
    <Text>
      {sample('Inline')} <Code>{sample('ruum ls /Projects')}</Code> {sample('code, and a hidden word for screen readers:')}
      <VisuallyHidden> {sample('(an agent)')}</VisuallyHidden>
    </Text>
  </Stack>
)

export const Lists: Story = () => (
  <Stack>
    <ProseList>
      <li>{sample('Upload files from any device.')}</li>
      <li>{sample('Every change is kept as a version.')}</li>
    </ProseList>
    <ProseList ordered size="sm">
      <li>{sample('Open Claude Desktop settings.')}</li>
      <li>{sample('Add the address.')}</li>
    </ProseList>
  </Stack>
)
