import type { Story } from '@ladle/react'
import { Row, Stack } from '../layout/Layout'
import { Badge, Chip, Tag, TagList, type BadgeTone } from './Badge'
import { tagColors } from './tagColor'
import { sample } from '../story'

export default { title: 'Badge, tag and chip' }

const tones: BadgeTone[] = ['neutral', 'primary', 'success', 'warning', 'danger', 'agent']

export const Badges: Story = () => (
  <Row>
    {tones.map((t) => (
      <Badge key={t} tone={t}>
        {t}
      </Badge>
    ))}
    <Badge tone="primary">{sample('Current')}</Badge>
    <Badge tone="success">{sample('Active')}</Badge>
    <Badge tone="warning">{sample('Paused')}</Badge>
    <Badge tone="danger">{sample('Access ended')}</Badge>
  </Row>
)

const tagHex: Record<string, string> = {
  spruce: '#1d5c57',
  moss: '#2b6439',
  clay: '#8a4718',
  brick: '#a3301c',
  ochre: '#7f5300',
  slate: '#4a5868',
  plum: '#6e3b5e',
  sand: '#8a7a55',
}

export const Tags: Story = () => (
  <Stack>
    <TagList label={sample('Tags')}>
      {tagColors.map((c) => (
        <Tag key={c} name={c} color={tagHex[c]} />
      ))}
    </TagList>
    <TagList label={sample('Removable tags')}>
      <Tag name="Invoices" color="#33aa77" onRemove={() => {}} />
      <Tag name="No colour" onRemove={() => {}} />
      <Tag name="Free colour #f0f" color="#f0f" />
    </TagList>
  </Stack>
)

export const Chips: Story = () => (
  <Row>
    <Chip kind="hidden" srText="Hidden secret value" title={sample('Hidden. Only people who can manage garden see secret values.')}>
      {sample('hidden: secret')}
    </Chip>
    <Chip kind="hidden" srText="Hidden personal value">
      {sample('hidden: personal')}
    </Chip>
    <Chip kind="standin" srText="Stand-in for a personal value, person 8 f 3 a 9 1">
      {sample('person 8f3a91')}
    </Chip>
    <Chip kind="value">{sample('value 1')}</Chip>
  </Row>
)
