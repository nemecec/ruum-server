import type { Story } from '@ladle/react'
import { Row, Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { Button, ButtonAnchor, ButtonLink, IconButton } from './Button'

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
        <Button variant={v}>Save</Button>
        <Button variant={v} disabled>
          Disabled
        </Button>
        <Button variant={v} busy disabled>
          Saving...
        </Button>
        <Button variant={v} size="sm">
          Small
        </Button>
        <Button variant={v} icon="upload">
          Upload files
        </Button>
      </Row>
    ))}
  </Stack>
)

export const Primary: Story = () => <Button variant="primary">Upload files</Button>
export const Secondary: Story = () => <Button>New folder</Button>
export const Quiet: Story = () => <Button variant="quiet">End now</Button>
export const Danger: Story = () => <Button variant="danger">Delete 12 items for good</Button>
export const Link: Story = () => <Button variant="link">Clear selection</Button>
export const Busy: Story = () => (
  <Button variant="primary" busy disabled>
    Deleting...
  </Button>
)
export const Disabled: Story = () => <Button disabled>Move</Button>
export const Wide: Story = () => (
  <Stack>
    <ButtonAnchor wide variant="primary" href="#signin">
      Sign in with email
    </ButtonAnchor>
    <ButtonLink wide to="/">
      Not now
    </ButtonLink>
  </Stack>
)
export const Icons: Story = () => (
  <Row>
    <IconButton icon="close" label="Close" />
    <IconButton icon="more" label="More actions for plan.txt" />
    <IconButton icon="copy" label="Copy the address" />
    <IconButton icon="close" label="Remove tag" size="sm" />
    <IconButton icon="close" label="Close (disabled)" disabled />
  </Row>
)
