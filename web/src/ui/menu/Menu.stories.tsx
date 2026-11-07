import type { Story } from '@ladle/react'
import { Row } from '../layout/Layout'
import { Menu } from './Menu'

export default { title: 'Menu' }

const roles = [
  { label: 'Can view', hint: 'See and download', onSelect: () => {} },
  { label: 'Can comment', hint: 'See and comment', onSelect: () => {} },
  { label: 'Can edit', hint: 'Change, upload and rename', onSelect: () => {}, checked: true },
  { label: 'Can manage', hint: 'Also move, delete and share', onSelect: () => {}, disabled: true },
]

export const RoleMenu: Story = () => <Menu label="Can edit" ariaLabel="Access of Liis Tamm" items={roles} />

export const RowMenu: Story = () => (
  <Row justify="end">
    <Menu
      label="More"
      ariaLabel="More actions for plan.txt"
      quiet
      align="end"
      items={[
        { label: 'Rename', onSelect: () => {} },
        { label: 'Move', onSelect: () => {} },
        { label: 'Move to trash', onSelect: () => {}, danger: true },
      ]}
    />
  </Row>
)
