import type { Story } from '@ladle/react'
import { Button } from '../button/Button'
import { Text } from '../text/Text'
import { Actions, Card, Columns, Grow, Indent, Page, PageBody, Row, Stack, Toolbar } from './Layout'
import { sample } from '../story'

export default { title: 'Layout' }

export const StackAndRow: Story = () => (
  <Stack gap={5}>
    {([1, 3, 5] as const).map((g) => (
      <Stack key={g} gap={g}>
        <Text>{sample('Stack, gap')} {g}</Text>
        <Text tone="muted">{sample('Second line')}</Text>
      </Stack>
    ))}
    <Row gap={3}>
      <Text as="span">{sample('A row')}</Text>
      <Grow>
        <Text as="span" tone="muted">
          {sample('Grow takes the free space')}
        </Text>
      </Grow>
      <Button size="sm">{sample('Action')}</Button>
    </Row>
  </Stack>
)

export const ActionsAndToolbar: Story = () => (
  <Stack gap={5}>
    <Toolbar>
      <h1>{sample('Files')}</h1>
      <Actions>
        <Button>{sample('New folder')}</Button>
        <Button variant="primary">{sample('Upload files')}</Button>
      </Actions>
    </Toolbar>
    <Actions align="start">
      <Button variant="primary">{sample('Start')}</Button>
      <Button>{sample('Second')}</Button>
    </Actions>
    <Actions align="center">
      <Button>{sample('Centred')}</Button>
    </Actions>
  </Stack>
)

export const Cards: Story = () => (
  <Columns>
    {['Profile', 'Sign-in methods', 'Signed-in devices'].map((t) => (
      <Card key={t} aria-label={t}>
        <h2>{t}</h2>
        <Text tone="muted">{sample('A card holds one topic of a page.')}</Text>
        <Indent>
          <Text size="sm">{sample('An indented detail')}</Text>
        </Indent>
      </Card>
    ))}
  </Columns>
)

export const Pages: Story = () => (
  <Stack gap={5}>
    <Page narrow as="div">
      <h1>{sample('Sign in to Ruum')}</h1>
      <Text tone="muted">{sample('A narrow page outside the app shell.')}</Text>
    </Page>
    <PageBody narrow>
      <h1>{sample('Account')}</h1>
      <Text>{sample('The body of a page inside the shell, with the reading width.')}</Text>
    </PageBody>
  </Stack>
)
