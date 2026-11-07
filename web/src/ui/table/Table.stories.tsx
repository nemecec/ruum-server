import type { Story } from '@ladle/react'
import { useState } from 'react'
import { AgentName } from '../agent/AgentName'
import { Button } from '../button/Button'
import { BareCheckbox } from '../field/Field'
import { KindIcon } from '../icon/Icon'
import { Text, VisuallyHidden } from '../text/Text'
import { NameCell, SortHeader, Table, TBody, Td, Th, THead, Tr } from './Table'

export default { title: 'Table and file list' }

const rows = [
  { id: '1', kind: 'folder', name: 'Clients', size: '-', when: 'Yesterday', who: 'Liis Tamm', agent: false },
  { id: '2', kind: 'file', name: 'plan.txt', size: '2 KB', when: '2 h ago', who: 'Claude for Liis', agent: true },
  { id: '3', kind: 'file', name: 'site-photo.jpg', size: '3.4 MB', when: '5 Nov 2026', who: 'Jonas Kask', agent: false },
]

export const FileList: Story = () => {
  const [sort, setSort] = useState<'ascending' | 'descending'>('ascending')
  const [checked, setChecked] = useState<string[]>(['2'])
  return (
    <Table caption="Contents of this folder">
      <THead>
        <Tr>
          <Th scope="col" check>
            <BareCheckbox
              aria-label="Select all"
              checked={checked.length === rows.length}
              onChange={(e) => setChecked(e.target.checked ? rows.map((r) => r.id) : [])}
            />
          </Th>
          <SortHeader label="Name" sort={sort} onSort={() => setSort(sort === 'ascending' ? 'descending' : 'ascending')} />
          <SortHeader label="Size" sort="none" onSort={() => {}} hideNarrow />
          <SortHeader label="Changed" sort="none" onSort={() => {}} hideNarrow />
          <Th scope="col">
            <VisuallyHidden>Actions</VisuallyHidden>
          </Th>
        </Tr>
      </THead>
      <TBody>
        {rows.map((r) => (
          <Tr key={r.id} selected={checked.includes(r.id)} dropTarget={r.id === '1'}>
            <Td check>
              <BareCheckbox
                aria-label={`Select ${r.name}`}
                checked={checked.includes(r.id)}
                onChange={(e) => setChecked(e.target.checked ? [...checked, r.id] : checked.filter((x) => x !== r.id))}
              />
            </Td>
            <Th scope="row">
              <NameCell icon={<KindIcon kind={r.kind} name={r.name} />}>
                <Button variant="link">{r.name}</Button>
              </NameCell>
            </Th>
            <Td hideNarrow numeric>
              {r.size}
            </Td>
            <Td hideNarrow>
              {r.when}
              <Text as="span" tone="muted" size="sm" block>
                {r.agent ? <AgentName srText=" (an agent)">{r.who}</AgentName> : r.who}
              </Text>
            </Td>
            <Td actions>
              <Button size="sm" aria-label={`Details of ${r.name}`}>
                Details
              </Button>
            </Td>
          </Tr>
        ))}
      </TBody>
    </Table>
  )
}

export const VisibleCaption: Story = () => (
  <Table caption="Members" showCaption>
    <THead>
      <Tr>
        <Th scope="col">Name</Th>
        <Th scope="col">Role</Th>
      </Tr>
    </THead>
    <TBody>
      <Tr>
        <Th scope="row">Ada Lovelace</Th>
        <Td>Owner</Td>
      </Tr>
    </TBody>
  </Table>
)
