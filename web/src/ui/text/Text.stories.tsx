import type { Story } from '@ladle/react'
import { Stack } from '../layout/Layout'
import { Code, ProseList, Text, VisuallyHidden, type Tone } from './Text'

export default { title: 'Text' }

const tones: Tone[] = ['default', 'muted', 'faint', 'primary', 'success', 'warning', 'danger', 'agent']

export const Headings: Story = () => (
  <Stack>
    <h1>Page title (h1)</h1>
    <h2>Panel and dialog title (h2)</h2>
    <h3>Section title (h3)</h3>
    <Text>Body text in the base size. Ruum keeps people's work and lets agents touch it.</Text>
  </Stack>
)

export const Tones: Story = () => (
  <Stack gap={2}>
    {tones.map((t) => (
      <Text key={t} tone={t}>
        {t}: The quick brown fox, 12 files, 2 h ago
      </Text>
    ))}
  </Stack>
)

export const Sizes: Story = () => (
  <Stack gap={2}>
    {(['xs', 'sm', 'md', 'lg'] as const).map((s) => (
      <Text key={s} size={s}>
        Size {s}
      </Text>
    ))}
    <Text weight="strong">Strong</Text>
    <Text mono>Mono: key prefix 8f3a91</Text>
    <Text italic tone="faint">
      NULL
    </Text>
    <Text numeric>1,234.50 · 98.10</Text>
  </Stack>
)

export const Truncated: Story = () => (
  <Stack>
    <Text truncate>A very long file name that does not fit on one line and is cut with an ellipsis at the end.pdf</Text>
    <Text>
      Inline <Code>ruum ls /Projects</Code> code, and a hidden word for screen readers:
      <VisuallyHidden> (an agent)</VisuallyHidden>
    </Text>
  </Stack>
)

export const Lists: Story = () => (
  <Stack>
    <ProseList>
      <li>Upload files from any device.</li>
      <li>Every change is kept as a version.</li>
    </ProseList>
    <ProseList ordered size="sm">
      <li>Open Claude Desktop settings.</li>
      <li>Add the address.</li>
    </ProseList>
  </Stack>
)
