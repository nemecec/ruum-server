import type { Story } from '@ladle/react'
import { RuumApiError } from '../../api/client'
import { Button } from '../button/Button'
import { Stack } from '../layout/Layout'
import { Banner, EmptyState, ErrorNote, Loaded, Loading, Meter, Note, Notice, ProgressBar, Skeleton, Spinner } from './Feedback'
import { sample } from '../story'

export default { title: 'Feedback' }

export const Notes: Story = () => (
  <Stack>
    <Note>{sample('Info: Copy the link now. For safety, Ruum shows it only once.')}</Note>
    <Note tone="success">{sample('Success: 12 files uploaded to Pärna 12.')}</Note>
    <Note tone="warning">{sample('Warning: Agents will see the real values of phone.')}</Note>
    <Note tone="danger">{sample('Error: Ruum couldn\'t load this folder.')}</Note>
    <Note tone="info" action={<Button size="sm">{sample('Try again')}</Button>}>
      {sample('With an action.')}
    </Note>
  </Stack>
)

const apiError = new RuumApiError(409, { code: 'name_taken', message: 'a node with this name exists', hint: sample('pick another name') })

export const Errors: Story = () => (
  <Stack>
    <ErrorNote error={apiError} context={{ name: 'plan.txt' }} />
    <ErrorNote error={new Error('Network down')} />
  </Stack>
)

export const LoadingStates: Story = () => (
  <Stack>
    <Loading />
    <Loading label={sample('Comparing')} />
    <Spinner />
    <Skeleton lines={4} />
    <Loaded state={{ data: undefined, error: undefined, loading: true }}>{() => null}</Loaded>
    <Loaded state={{ data: 'Loaded data', error: undefined, loading: false }}>{(d) => <p>{d}</p>}</Loaded>
  </Stack>
)

export const Empty: Story = () => (
  <Stack>
    <EmptyState icon="upload" title={sample('Your files go here')} text={sample('Upload files or make folders. Every change is kept as a version.')}>
      <Button variant="primary">{sample('Upload files')}</Button>
      <Button>{sample('New folder')}</Button>
    </EmptyState>
    <EmptyState title={sample('This folder is empty')} />
  </Stack>
)

export const StatusAndBanner: Story = () => (
  <Stack>
    <Banner>{sample('You\'re offline. Changes wait until you\'re back.')}</Banner>
    <Notice text={sample('Link copied.')} />
    <Notice text={null} />
  </Stack>
)

export const Progress: Story = () => (
  <Stack>
    <ProgressBar value={45} max={100} label={sample('plan.txt, 45%')} />
    <ProgressBar value={100} max={100} label={sample('photo.jpg, done')} />
    <Meter value={3} max={10} label={sample('Members')} />
    <Meter value={9.5} max={10} high={9} label={sample('Storage, nearly full')} />
  </Stack>
)
