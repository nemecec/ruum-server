import type { Story } from '@ladle/react'
import { Link } from 'react-router'
import { Button } from '../button/Button'
import { Stack } from '../layout/Layout'
import { Breadcrumbs, Crumb, CurrentCrumb, Tabs } from './Nav'

export default { title: 'Navigation' }

export const Crumbs: Story = () => (
  <Stack>
    <Breadcrumbs>
      <Crumb>
        <Link to="/files">Files</Link>
      </Crumb>
      <Crumb>
        <Link to="/files/clients">Clients</Link>
      </Crumb>
      <CurrentCrumb>Pärna 12</CurrentCrumb>
    </Breadcrumbs>
    <Breadcrumbs label="Folder path">
      <Crumb>
        <Button variant="link">Folders shared with you</Button>
      </Crumb>
      <CurrentCrumb current="location">Engine drawings</CurrentCrumb>
    </Breadcrumbs>
  </Stack>
)

export const TabBar: Story = () => (
  <Tabs
    label="Admin"
    items={[
      { to: '/', label: 'Members', end: true },
      { to: '/invitations', label: 'Invitations' },
      { to: '/policy', label: 'Sharing rules' },
      { to: '/limits', label: 'Plan' },
    ]}
  />
)
