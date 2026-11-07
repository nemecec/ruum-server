import type { Story } from '@ladle/react'
import { useState } from 'react'
import { Button } from '../button/Button'
import { ErrorNote } from '../feedback/Feedback'
import { Field, TextInput } from '../field/Field'
import { Actions, Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { ConfirmButton, ConfirmDialog, Dialog } from './Dialog'

export default { title: 'Dialog' }

function Form({ onClose, error }: { onClose: () => void; error?: unknown }) {
  return (
    <Stack as="form" gap={4} onSubmit={(e) => e.preventDefault()}>
      <Field label="Folder name">
        <TextInput placeholder="Site photos" />
      </Field>
      <ErrorNote error={error} />
      <Actions>
        <Button onClick={onClose}>Cancel</Button>
        <Button type="submit" variant="primary">
          Create
        </Button>
      </Actions>
    </Stack>
  )
}

export const FormDialog: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <>
      <Button onClick={() => setOpen(true)}>New folder</Button>
      <Dialog title="New folder" open={open} onClose={() => setOpen(false)}>
        <Form onClose={() => setOpen(false)} />
      </Dialog>
    </>
  )
}

export const WithError: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <Dialog title="New folder" open={open} onClose={() => setOpen(false)}>
      <Form onClose={() => setOpen(false)} error={new Error('Ruum could not save the folder.')} />
    </Dialog>
  )
}

export const Confirm: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <>
      <Button onClick={() => setOpen(true)}>Empty trash</Button>
      {open && (
        <ConfirmDialog
          title="Delete 12 items for good?"
          message="Everything in the trash is deleted, with its versions. You can't undo this."
          confirmLabel="Delete 12 items for good"
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
    label="Restore"
    title="Restore the version of 5 Nov, 14:00?"
    message="It becomes the current version. The version you have now stays in Versions."
    confirmLabel="Restore"
    onConfirm={() => {}}
  />
)
ConfirmButtonStory.storyName = 'Confirm button'

export const Wide: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <Dialog title="Share plan.txt" size="md" open={open} onClose={() => setOpen(false)}>
      <Text>A 720 px dialog (share).</Text>
    </Dialog>
  )
}

export const Full: Story = () => {
  const [open, setOpen] = useState(true)
  return (
    <Dialog title="Compare versions" size="full" open={open} onClose={() => setOpen(false)}>
      <Text>A wide dialog; a full page on the phone.</Text>
    </Dialog>
  )
}
