import type { Story } from '@ladle/react'
import { Link } from 'react-router'
import { Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { AgentName } from './AgentName'
import { sample } from '../story'

export default { title: 'Agent name' }

export const Default: Story = () => <AgentName srText=" (an agent)">{sample('Claude Code (agent of Ann)')}</AgentName>

export const WithLink: Story = () => (
  <AgentName>
    <Link to="/agents/1">{sample('Claude Desktop')}</Link>
  </AgentName>
)

export const InText: Story = () => (
  <Stack>
    <Text>
      {sample('Changed 2 hours ago by')} <AgentName srText=" (an agent)">{sample('Claude Desktop (agent of Liis Tamm)')}</AgentName>
    </Text>
    <Text tone="muted" size="sm">
      <AgentName>{sample('A very long agent name that wraps onto the next line on a phone (agent of Jonas Kask)')}</AgentName>
    </Text>
  </Stack>
)
