import type { Story } from '@ladle/react'
import { Row } from '../layout/Layout'
import { Menu } from './Menu'
import { sample } from '../story'

export default { title: 'Menu' }

const roles = [
  { label: sample('Can view'), hint: sample('See and download'), onSelect: () => {} },
  { label: sample('Can comment'), hint: sample('See and comment'), onSelect: () => {} },
  { label: sample('Can edit'), hint: sample('Change, upload and rename'), onSelect: () => {}, checked: true },
  { label: sample('Can manage'), hint: sample('Also move, delete and share'), onSelect: () => {}, disabled: true },
]

// The name starts with the visible label (WCAG 2.5.3): "Can edit Access of Liis Tamm".
export const RoleMenu: Story = () => <Menu label={sample('Can edit')} context={sample('Access of Liis Tamm')} items={roles} />

export const RowMenu: Story = () => (
  <Row justify="end">
    <Menu
      icon="more"
      ariaLabel={sample('More actions for plan.txt')}
      quiet
      align="end"
      items={[
        { label: sample('Rename'), onSelect: () => {} },
        { label: sample('Move'), onSelect: () => {} },
        { label: sample('Move to trash'), onSelect: () => {}, danger: true },
      ]}
    />
  </Row>
)
