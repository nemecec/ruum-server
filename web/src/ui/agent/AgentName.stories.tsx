import type { Story } from '@ladle/react'
import { Link } from 'react-router'
import { Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { AgentName } from './AgentName'

export default { title: 'Agent name' }

export const Default: Story = () => <AgentName srText=" (an agent)">Claude Code (agent of Ann)</AgentName>

export const WithLink: Story = () => (
  <AgentName>
    <Link to="/agents/1">Claude Desktop</Link>
  </AgentName>
)

export const InText: Story = () => (
  <Stack>
    <Text>
      Changed 2 h ago by <AgentName srText=" (an agent)">Claude for Liis</AgentName>
    </Text>
    <Text tone="muted" size="sm">
      <AgentName>A very long agent name that wraps onto the next line on a phone (agent of Jonas Kask)</AgentName>
    </Text>
  </Stack>
)
