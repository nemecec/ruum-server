import type { Story } from '@ladle/react'
import { useState } from 'react'
import { Stack } from '../layout/Layout'
import { Checkbox, CopyField, Field, Fieldset, Radio, SecretBox, Select, Switch, TextArea, TextInput } from './Field'
import { sample } from '../story'

export default { title: 'Form fields' }

export const TextFields: Story = () => (
  <Stack gap={4}>
    <Field label={sample('Folder name')}>
      <TextInput placeholder={sample('Site photos')} />
    </Field>
    <Field label={sample('Email')} hint={sample('We send the invitation to this address.')}>
      <TextInput type="email" autoComplete="email" width="md" />
    </Field>
    <Field label={sample('Name')} error={sample('A file with this name is already here.')} errorId="name-error">
      <TextInput defaultValue="plan.txt" aria-invalid="true" aria-describedby="name-error" />
    </Field>
    <Field label={sample('Ends on')} hint={sample('Format: dd.mm.yyyy')}>
      <TextInput type="date" width="sm" />
    </Field>
    <Field label={sample('Disabled')} hint={sample('You\'re offline.')}>
      <TextInput disabled defaultValue="Can't change" />
    </Field>
    <Field label={sample('Read only')}>
      <TextInput readOnly defaultValue="https://ruum.localhost/mcp" mono />
    </Field>
    <Field label={sample('Query')}>
      <TextArea mono defaultValue="select * from plants where family = $1" />
    </Field>
    <Field label={sample('Space')} labelHidden>
      <TextInput defaultValue="A field with a hidden label" />
    </Field>
  </Stack>
)

export const Selects: Story = () => (
  <Stack>
    <Field label={sample('Role')}>
      <Select defaultValue="edit">
        <option value="view">{sample('Can view')}</option>
        <option value="edit">{sample('Can edit')}</option>
        <option value="manage">{sample('Can manage')}</option>
      </Select>
    </Field>
    <Field label={sample('Disabled role')}>
      <Select disabled>
        <option>{sample('Can view')}</option>
      </Select>
    </Field>
  </Stack>
)

export const ChecksAndRadios: Story = () => (
  <Stack gap={4}>
    <Fieldset legend={sample('Sharing')}>
      <Checkbox label={sample('Members can share')} defaultChecked />
      <Checkbox label={sample('Allow guests')} />
      <Checkbox label={sample('Allow links that can edit (disabled)')} disabled />
    </Fieldset>
    <Fieldset legend={sample('What should Ruum do?')}>
      <Radio name="c" label={sample('Save as a new version')} defaultChecked />
      <Radio name="c" label={sample('Keep both, as two separate files')} />
      <Radio name="c" label={sample('Skip')} />
    </Fieldset>
    <Fieldset legend={sample('Plain fieldset')} plain>
      <Radio name="d" label={sample('One')} defaultChecked />
      <Radio name="d" label={sample('Two')} />
    </Fieldset>
  </Stack>
)

export const Switches: Story = () => {
  const [on, setOn] = useState(true)
  return (
    <Stack>
      <Switch label={sample('Anyone with the link')} checked={on} onChange={setOn} />
      <Switch label={sample('Off')} checked={false} onChange={() => {}} />
      <Switch label={sample('Disabled')} checked onChange={() => {}} disabled />
    </Stack>
  )
}

export const CopyAndSecret: Story = () => (
  <Stack gap={4}>
    <CopyField label={sample('MCP address')} value="https://ruum.localhost/mcp" />
    <SecretBox label={sample('API key')} secret="example-api-key-not-real-0001" />
  </Stack>
)
