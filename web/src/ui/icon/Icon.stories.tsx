import type { Story } from '@ladle/react'
import { AgentName } from '../agent/AgentName'
import { Row, Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { AgentMark, FileIcon, FolderIcon, Icon, KindIcon } from './Icon'
import { iconNames } from './paths'
import { sample } from '../story'

export default { title: 'Icon' }

export const AllIcons: Story = () => (
  <Row gap={4}>
    {iconNames.map((n) => (
      <Stack key={n} gap={1} align="center">
        <Icon name={n} />
        <Text size="xs" tone="muted">
          {n}
        </Text>
      </Stack>
    ))}
  </Row>
)

export const Small: Story = () => (
  <Row>
    {iconNames.map((n) => (
      <Icon key={n} name={n} small />
    ))}
  </Row>
)

export const Kinds: Story = () => (
  <Row gap={4}>
    <FolderIcon />
    <FileIcon />
    <FileIcon type="image/png" />
    <KindIcon kind="folder" />
    <KindIcon kind="file" name="photo.jpg" />
  </Row>
)

export const TheAgentMark: Story = () => (
  <Stack>
    <AgentMark />
    <AgentName srText=", an agent for Liis Tamm">{sample('Claude Desktop (agent of Liis Tamm)')}</AgentName>
  </Stack>
)
