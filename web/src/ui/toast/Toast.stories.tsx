import type { Story } from '@ladle/react'
import { Button } from '../button/Button'
import { Stack } from '../layout/Layout'
import { ToastProvider, ToastRegion, ToastView } from './Toast'
import { useToast } from './useToast'
import { sample } from '../story'

export default { title: 'Toast' }

export const Kinds: Story = () => (
  <ToastRegion>
    <ToastView text={sample('Folder Site photos created.')} onDismiss={() => {}} />
    <ToastView text={sample('Moved plan.txt to the trash.')} onDismiss={() => {}}>
      <Button size="sm">{sample('Undo')}</Button>
    </ToastView>
    <ToastView text={sample('Ruum couldn\'t undo this.')} error onDismiss={() => {}} />
  </ToastRegion>
)

function Trigger() {
  const { toast } = useToast()
  return (
    <Stack>
      <Button onClick={() => toast('Renamed to plan-v2.txt.')}>{sample('Show a toast')}</Button>
      <Button onClick={() => toast('Moved 3 items to the trash.', { undo: async () => 'Restored 3 items.' })}>
        {sample('Show a toast with Undo')}
      </Button>
      <Button onClick={() => toast('Team space is ready.', { action: { label: sample('Invite people'), run: () => {} } })}>
        {sample('Show a toast with an action')}
      </Button>
    </Stack>
  )
}

export const Live: Story = () => (
  <ToastProvider>
    <Trigger />
  </ToastProvider>
)
