import type { Story } from '@ladle/react'
import { useState } from 'react'
import { Stack } from '../layout/Layout'
import { Checkbox, CopyField, Field, Fieldset, Radio, SecretBox, Select, Switch, TextArea, TextInput } from './Field'

export default { title: 'Form fields' }

export const TextFields: Story = () => (
  <Stack gap={4}>
    <Field label="Folder name">
      <TextInput placeholder="Site photos" />
    </Field>
    <Field label="Email" hint="We send the invitation to this address.">
      <TextInput type="email" autoComplete="email" width="md" />
    </Field>
    <Field label="Name" error="A file with this name is already here." errorId="name-error">
      <TextInput defaultValue="plan.txt" aria-invalid="true" aria-describedby="name-error" />
    </Field>
    <Field label="Ends on" hint="Format: dd.mm.yyyy">
      <TextInput type="date" width="sm" />
    </Field>
    <Field label="Disabled" hint="You're offline.">
      <TextInput disabled defaultValue="Can't change" />
    </Field>
    <Field label="Read only">
      <TextInput readOnly defaultValue="https://ruum.localhost/mcp" mono />
    </Field>
    <Field label="Query">
      <TextArea mono defaultValue="select * from plants where family = $1" />
    </Field>
    <Field label="Space" labelHidden>
      <TextInput defaultValue="A field with a hidden label" />
    </Field>
  </Stack>
)

export const Selects: Story = () => (
  <Stack>
    <Field label="Role">
      <Select defaultValue="edit">
        <option value="view">Can view</option>
        <option value="edit">Can edit</option>
        <option value="manage">Can manage</option>
      </Select>
    </Field>
    <Field label="Disabled role">
      <Select disabled>
        <option>Can view</option>
      </Select>
    </Field>
  </Stack>
)

export const ChecksAndRadios: Story = () => (
  <Stack gap={4}>
    <Fieldset legend="Sharing">
      <Checkbox label="Members can share" defaultChecked />
      <Checkbox label="Allow guests" />
      <Checkbox label="Allow links that can edit (disabled)" disabled />
    </Fieldset>
    <Fieldset legend="What should Ruum do?">
      <Radio name="c" label="Save as a new version" defaultChecked />
      <Radio name="c" label="Keep both, as two separate files" />
      <Radio name="c" label="Skip" />
    </Fieldset>
    <Fieldset legend="Plain fieldset" plain>
      <Radio name="d" label="One" defaultChecked />
      <Radio name="d" label="Two" />
    </Fieldset>
  </Stack>
)

export const Switches: Story = () => {
  const [on, setOn] = useState(true)
  return (
    <Stack>
      <Switch label="Anyone with the link" checked={on} onChange={setOn} />
      <Switch label="Off" checked={false} onChange={() => {}} />
      <Switch label="Disabled" checked onChange={() => {}} disabled />
    </Stack>
  )
}

export const CopyAndSecret: Story = () => (
  <Stack gap={4}>
    <CopyField label="MCP address" value="https://ruum.localhost/mcp" />
    <SecretBox label="API key" secret="example-api-key-not-real-0001" />
  </Stack>
)
