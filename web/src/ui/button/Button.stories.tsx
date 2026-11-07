import type { Story } from '@ladle/react'
import { Row, Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { Button, ButtonAnchor, ButtonLink, IconButton } from './Button'
import { sample } from '../story'

export default { title: 'Button' }

const variants = ['primary', 'secondary', 'quiet', 'danger', 'link'] as const

/** Each kind in each state: default, disabled, busy, small, with an icon. */
export const AllStates: Story = () => (
  <Stack gap={4}>
    {variants.map((v) => (
      <Row key={v} gap={3}>
        <Text as="span" tone="muted" size="sm">
          {v}
        </Text>
        <Button variant={v}>{sample('Save')}</Button>
        <Button variant={v} disabled>
          {sample('Disabled')}
        </Button>
        <Button variant={v} busy disabled>
          {sample('Saving...')}
        </Button>
        <Button variant={v} size="sm">
          {sample('Small')}
        </Button>
        <Button variant={v} icon="upload">
          {sample('Upload files')}
        </Button>
      </Row>
    ))}
  </Stack>
)

export const Primary: Story = () => <Button variant="primary">{sample('Upload files')}</Button>
export const Secondary: Story = () => <Button>{sample('New folder')}</Button>
export const Quiet: Story = () => <Button variant="quiet">{sample('End now')}</Button>
export const Danger: Story = () => <Button variant="danger">{sample('Delete 12 items for good')}</Button>
export const Link: Story = () => <Button variant="link">{sample('Clear selection')}</Button>
export const Busy: Story = () => (
  <Button variant="primary" busy disabled>
    {sample('Deleting...')}
  </Button>
)
export const Disabled: Story = () => <Button disabled>{sample('Move')}</Button>
export const Wide: Story = () => (
  <Stack>
    <ButtonAnchor wide variant="primary" href="#signin">
      {sample('Sign in with email')}
    </ButtonAnchor>
    <ButtonLink wide to="/">
      {sample('Not now')}
    </ButtonLink>
  </Stack>
)
export const Icons: Story = () => (
  <Row>
    <IconButton icon="close" label={sample('Close')} />
    <IconButton icon="more" label={sample('More actions for plan.txt')} />
    <IconButton icon="copy" label={sample('Copy the address')} />
    <IconButton icon="close" label={sample('Remove tag')} size="sm" />
    <IconButton icon="close" label={sample('Close (disabled)')} disabled />
  </Row>
)
