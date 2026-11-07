import type { Story } from '@ladle/react'
import { Button } from '../button/Button'
import { Stack } from '../layout/Layout'
import { ToastProvider, ToastRegion, ToastView } from './Toast'
import { useToast } from './useToast'

export default { title: 'Toast' }

export const Kinds: Story = () => (
  <ToastRegion>
    <ToastView text="Folder Site photos created." onDismiss={() => {}} />
    <ToastView text="Moved plan.txt to the trash." onDismiss={() => {}}>
      <Button size="sm">Undo</Button>
    </ToastView>
    <ToastView text="Ruum couldn't undo this." error onDismiss={() => {}} />
  </ToastRegion>
)

function Trigger() {
  const { toast } = useToast()
  return (
    <Stack>
      <Button onClick={() => toast('Renamed to plan-v2.txt.')}>Show a toast</Button>
      <Button onClick={() => toast('Moved 3 items to the trash.', { undo: async () => 'Restored 3 items.' })}>
        Show a toast with Undo
      </Button>
      <Button onClick={() => toast('Team space is ready.', { action: { label: 'Invite people', run: () => {} } })}>
        Show a toast with an action
      </Button>
    </Stack>
  )
}

export const Live: Story = () => (
  <ToastProvider>
    <Trigger />
  </ToastProvider>
)
