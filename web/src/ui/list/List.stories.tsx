import type { Story } from '@ladle/react'
import { Badge } from '../badge/Badge'
import { Button } from '../button/Button'
import { FolderIcon } from '../icon/Icon'
import { Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { Fact, Facts, List, ListEmpty, ListRow, ListRowMain, LoadMore, Picker, PickerEmpty, PickerItem } from './List'
import { sample } from '../story'

export default { title: 'List and facts' }

export const Rows: Story = () => (
  <List>
    <ListRow>
      <ListRowMain>
        {sample('Firefox on macOS')} <Badge tone="primary">{sample('This device')}</Badge>
      </ListRowMain>
      <Text as="span" tone="muted" size="sm">
        {sample('last seen 2 min ago')}
      </Text>
    </ListRow>
    <ListRow>
      <ListRowMain>{sample('Safari on iPhone')}</ListRowMain>
      <Button size="sm">{sample('Sign out')}</Button>
    </ListRow>
    <ListRow undone>
      <ListRowMain>
        {sample('Renamed plan.txt')} <Badge>{sample('Undone')}</Badge>
      </ListRowMain>
    </ListRow>
  </List>
)

export const Empty: Story = () => (
  <List>
    <ListEmpty>{sample('No app passwords.')}</ListEmpty>
  </List>
)

export const Blocks: Story = () => (
  <List ordered>
    <ListRow block current>
      <strong>{sample('13 minutes ago (15:13)')}</strong>
      <Text as="div" tone="muted" size="sm">
        {sample('Liis Tamm - 2 KB')}
      </Text>
    </ListRow>
    <ListRow block>
      <strong>{sample('Yesterday at 18:20')}</strong>
      <Text as="div" tone="muted" size="sm">
        {sample('Claude Desktop (agent of Liis Tamm) · 1 KB')}
      </Text>
    </ListRow>
  </List>
)

export const FactList: Story = () => (
  <Facts>
    <Fact label={sample('Type')}>{sample('Text file')}</Fact>
    <Fact label={sample('Size')}>{sample('2 KB')}</Fact>
    <Fact label={sample('Changed')}>{sample('2 hours ago by Liis Tamm')}</Fact>
    <Fact label={sample('A long label for a fact')}>{sample('A long value that wraps onto the next line when the panel is narrow')}</Fact>
  </Facts>
)

export const FolderPicker: Story = () => (
  <Stack>
    <Picker label={sample('Folders')}>
      <PickerItem icon={<FolderIcon />} onClick={() => {}}>
        {sample('Clients')}
      </PickerItem>
      <PickerItem icon={<FolderIcon />} onClick={() => {}} disabled>
        {sample('Site photos (the folder itself)')}
      </PickerItem>
    </Picker>
    <Picker label={sample('Empty folders')}>
      <PickerEmpty>{sample('No folders here.')}</PickerEmpty>
    </Picker>
  </Stack>
)

export const More: Story = () => (
  <Stack>
    <LoadMore state={{ data: { nextCursor: 'x' }, loadMore: () => {}, loadingMore: false, moreError: undefined }} />
    <LoadMore state={{ data: { nextCursor: 'x' }, loadMore: () => {}, loadingMore: true, moreError: undefined }} />
  </Stack>
)
