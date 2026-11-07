import type { Story } from '@ladle/react'
import { Link } from 'react-router'
import { Button } from '../button/Button'
import { Stack } from '../layout/Layout'
import { Breadcrumbs, Crumb, CurrentCrumb, Tabs } from './Nav'
import { sample } from '../story'

export default { title: 'Navigation' }

export const Crumbs: Story = () => (
  <Stack>
    <Breadcrumbs>
      <Crumb>
        <Link to="/files">{sample('Files')}</Link>
      </Crumb>
      <Crumb>
        <Link to="/files/clients">{sample('Clients')}</Link>
      </Crumb>
      <CurrentCrumb>{sample('Pärna 12')}</CurrentCrumb>
    </Breadcrumbs>
    <Breadcrumbs label={sample('Folder path')}>
      <Crumb>
        <Button variant="link">{sample('Folders shared with you')}</Button>
      </Crumb>
      <CurrentCrumb current="location">{sample('Engine drawings')}</CurrentCrumb>
    </Breadcrumbs>
  </Stack>
)

export const TabBar: Story = () => (
  <Tabs
    label={sample('Admin')}
    items={[
      { to: '/', label: sample('Members'), end: true },
      { to: '/invitations', label: sample('Invitations') },
      { to: '/policy', label: sample('Sharing rules') },
      { to: '/limits', label: sample('Plan') },
    ]}
  />
)
