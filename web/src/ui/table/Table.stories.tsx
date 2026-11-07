import type { Story } from '@ladle/react'
import { useState } from 'react'
import { AgentName } from '../agent/AgentName'
import { Button } from '../button/Button'
import { BareCheckbox } from '../field/Field'
import { KindIcon } from '../icon/Icon'
import { Text, VisuallyHidden } from '../text/Text'
import { NameCell, SortHeader, Table, TBody, Td, Th, THead, Tr } from './Table'
import { sample } from '../story'

export default { title: 'Table and file list' }

const rows = [
  { id: '1', kind: 'folder', name: 'Clients', size: '-', when: 'Yesterday', who: 'Liis Tamm', agent: false },
  { id: '2', kind: 'file', name: 'plan.txt', size: '2 KB', when: '2 hours ago', who: 'Claude Desktop (agent of Liis Tamm)', agent: true },
  { id: '3', kind: 'file', name: 'site-photo.jpg', size: '3.4 MB', when: '5 Nov 2026', who: 'Jonas Kask', agent: false },
]

export const FileList: Story = () => {
  const [sort, setSort] = useState<'ascending' | 'descending'>('ascending')
  const [checked, setChecked] = useState<string[]>(['2'])
  return (
    <Table caption={sample('Contents of this folder')}>
      <THead>
        <Tr>
          <Th scope="col" check>
            <BareCheckbox
              aria-label={sample('Select all')}
              checked={checked.length === rows.length}
              onChange={(e) => setChecked(e.target.checked ? rows.map((r) => r.id) : [])}
            />
          </Th>
          <SortHeader label={sample('Name')} sort={sort} onSort={() => setSort(sort === 'ascending' ? 'descending' : 'ascending')} />
          <SortHeader label={sample('Size')} sort="none" onSort={() => {}} hideNarrow />
          <SortHeader label={sample('Changed')} sort="none" onSort={() => {}} hideNarrow />
          <Th scope="col">
            <VisuallyHidden>{sample('Actions')}</VisuallyHidden>
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
                {sample('Details')}
              </Button>
            </Td>
          </Tr>
        ))}
      </TBody>
    </Table>
  )
}

export const VisibleCaption: Story = () => (
  <Table caption={sample('Members')} showCaption>
    <THead>
      <Tr>
        <Th scope="col">{sample('Name')}</Th>
        <Th scope="col">{sample('Role')}</Th>
      </Tr>
    </THead>
    <TBody>
      <Tr>
        <Th scope="row">{sample('Ada Lovelace')}</Th>
        <Td>{sample('Owner')}</Td>
      </Tr>
    </TBody>
  </Table>
)
