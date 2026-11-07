import type { Story } from '@ladle/react'
import { Button } from '../button/Button'
import { Text } from '../text/Text'
import { Actions, Card, Columns, Grow, Indent, Page, PageBody, Row, Stack, Toolbar } from './Layout'

export default { title: 'Layout' }

export const StackAndRow: Story = () => (
  <Stack gap={5}>
    {([1, 3, 5] as const).map((g) => (
      <Stack key={g} gap={g}>
        <Text>Stack, gap {g}</Text>
        <Text tone="muted">Second line</Text>
      </Stack>
    ))}
    <Row gap={3}>
      <Text as="span">A row</Text>
      <Grow>
        <Text as="span" tone="muted">
          Grow takes the free space
        </Text>
      </Grow>
      <Button size="sm">Action</Button>
    </Row>
  </Stack>
)

export const ActionsAndToolbar: Story = () => (
  <Stack gap={5}>
    <Toolbar>
      <h1>Files</h1>
      <Actions>
        <Button>New folder</Button>
        <Button variant="primary">Upload files</Button>
      </Actions>
    </Toolbar>
    <Actions align="start">
      <Button variant="primary">Start</Button>
      <Button>Second</Button>
    </Actions>
    <Actions align="center">
      <Button>Centred</Button>
    </Actions>
  </Stack>
)

export const Cards: Story = () => (
  <Columns>
    {['Profile', 'Sign-in methods', 'Signed-in devices'].map((t) => (
      <Card key={t} aria-label={t}>
        <h2>{t}</h2>
        <Text tone="muted">A card holds one topic of a page.</Text>
        <Indent>
          <Text size="sm">An indented detail</Text>
        </Indent>
      </Card>
    ))}
  </Columns>
)

export const Pages: Story = () => (
  <Stack gap={5}>
    <Page narrow as="div">
      <h1>Sign in to Ruum</h1>
      <Text tone="muted">A narrow page outside the app shell.</Text>
    </Page>
    <PageBody narrow>
      <h1>Account</h1>
      <Text>The body of a page inside the shell, with the reading width.</Text>
    </PageBody>
  </Stack>
)
