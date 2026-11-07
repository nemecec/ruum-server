import type { Story } from '@ladle/react'
import { useState } from 'react'
import { Button } from '../button/Button'
import { ErrorNote } from '../feedback/Feedback'
import { Field, TextInput } from '../field/Field'
import { Actions, Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { ConfirmButton, ConfirmDialog, Dialog } from './Dialog'
import { sample } from '../story'

export default { title: 'Dialog' }

function Form({ onClose, error }: { onClose: () => void; error?: unknown }) {
  return (
    <Stack as="form" gap={4} onSubmit={(e) => e.preventDefault()}>
      <Field label={sample('Folder name')}>
        <TextInput placeholder={sample('Site photos')} />
      </Field>
      <ErrorNote error={error} />
      <Actions>
        <Button onClick={onClose}>{sample('Cancel')}</Button>
        <Button type="submit" variant="primary">
          {sample('Create')}
        </Button>
      </Actions>
    </Stack>
  )
}

export const FormDialog: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <>
      <Button onClick={() => setOpen(true)}>{sample('New folder')}</Button>
      <Dialog title={sample('New folder')} open={open} onClose={() => setOpen(false)}>
        <Form onClose={() => setOpen(false)} />
      </Dialog>
    </>
  )
}

export const WithError: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <Dialog title={sample('New folder')} open={open} onClose={() => setOpen(false)}>
      <Form onClose={() => setOpen(false)} error={new Error('Ruum could not save the folder.')} />
    </Dialog>
  )
}

export const Confirm: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <>
      <Button onClick={() => setOpen(true)}>{sample('Empty trash')}</Button>
      {open && (
        <ConfirmDialog
          title={sample('Delete 12 items for good?')}
          message={sample('Everything in the trash is deleted, with its versions. You can\'t undo this.')}
          confirmLabel={sample('Delete 12 items for good')}
          danger
          onConfirm={() => new Promise((r) => setTimeout(r, 1500))}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  )
}

export const ConfirmButtonStory: Story = () => (
  <ConfirmButton
    label={sample('Restore')}
    title={sample('Restore the version of 5 Nov, 14:00?')}
    message={sample('It becomes the current version. The version you have now stays in Versions.')}
    confirmLabel={sample('Restore')}
    onConfirm={() => {}}
  />
)
ConfirmButtonStory.storyName = 'Confirm button'

export const Wide: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <Dialog title={sample('Share plan.txt')} size="md" open={open} onClose={() => setOpen(false)}>
      <Text>{sample('A 720 px dialog (share).')}</Text>
    </Dialog>
  )
}

export const Full: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <Dialog title={sample('Compare versions')} size="full" open={open} onClose={() => setOpen(false)}>
      <Text>{sample('A wide dialog; a full page on the phone.')}</Text>
    </Dialog>
  )
}
