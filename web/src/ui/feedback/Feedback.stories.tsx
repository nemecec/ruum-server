import type { Story } from '@ladle/react'
import { RuumApiError } from '../../api/client'
import { Button } from '../button/Button'
import { Stack } from '../layout/Layout'
import { Banner, EmptyState, ErrorNote, Loaded, Loading, Meter, Note, Notice, ProgressBar, Skeleton, Spinner } from './Feedback'

export default { title: 'Feedback' }

export const Notes: Story = () => (
  <Stack>
    <Note>Info: Copy the link now. For safety, Ruum shows it only once.</Note>
    <Note tone="success">Success: 12 files uploaded to Pärna 12.</Note>
    <Note tone="warning">Warning: Agents will see the real values of phone.</Note>
    <Note tone="danger">Error: Ruum couldn't load this folder.</Note>
    <Note tone="info" action={<Button size="sm">Try again</Button>}>
      With an action.
    </Note>
  </Stack>
)

const apiError = new RuumApiError(409, { code: 'name_taken', message: 'a node with this name exists', hint: 'pick another name' })

export const Errors: Story = () => (
  <Stack>
    <ErrorNote error={apiError} context={{ name: 'plan.txt' }} />
    <ErrorNote error={new Error('Network down')} />
  </Stack>
)

export const LoadingStates: Story = () => (
  <Stack>
    <Loading />
    <Loading label="Comparing" />
    <Spinner />
    <Skeleton lines={4} />
    <Loaded state={{ data: undefined, error: undefined, loading: true }}>{() => null}</Loaded>
    <Loaded state={{ data: 'Loaded data', error: undefined, loading: false }}>{(d) => <p>{d}</p>}</Loaded>
  </Stack>
)

export const Empty: Story = () => (
  <Stack>
    <EmptyState icon="upload" title="Your files go here" text="Upload files or make folders. Every change is kept as a version.">
      <Button variant="primary">Upload files</Button>
      <Button>New folder</Button>
    </EmptyState>
    <EmptyState title="This folder is empty" />
  </Stack>
)

export const StatusAndBanner: Story = () => (
  <Stack>
    <Banner>You're offline. Changes wait until you're back.</Banner>
    <Notice text="Link copied." />
    <Notice text={null} />
  </Stack>
)

export const Progress: Story = () => (
  <Stack>
    <ProgressBar value={45} max={100} label="plan.txt, 45%" />
    <ProgressBar value={100} max={100} label="photo.jpg, done" />
    <Meter value={3} max={10} label="Members" />
    <Meter value={9.5} max={10} high={9} label="Storage, nearly full" />
  </Stack>
)
