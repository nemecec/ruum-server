import type { Story } from '@ladle/react'
import { Badge } from '../badge/Badge'
import { Button } from '../button/Button'
import { FolderIcon } from '../icon/Icon'
import { Stack } from '../layout/Layout'
import { Text } from '../text/Text'
import { Fact, Facts, List, ListEmpty, ListRow, ListRowMain, LoadMore, Picker, PickerEmpty, PickerItem } from './List'

export default { title: 'List and facts' }

export const Rows: Story = () => (
  <List>
    <ListRow>
      <ListRowMain>
        Firefox on macOS <Badge tone="primary">This device</Badge>
      </ListRowMain>
      <Text as="span" tone="muted" size="sm">
        last seen 2 min ago
      </Text>
    </ListRow>
    <ListRow>
      <ListRowMain>Safari on iPhone</ListRowMain>
      <Button size="sm">Sign out</Button>
    </ListRow>
    <ListRow undone>
      <ListRowMain>
        Renamed plan.txt <Badge>Undone</Badge>
      </ListRowMain>
    </ListRow>
  </List>
)

export const Empty: Story = () => (
  <List>
    <ListEmpty>No app passwords.</ListEmpty>
  </List>
)

export const Blocks: Story = () => (
  <List ordered>
    <ListRow block current>
      <strong>13 minutes ago (15:13)</strong>
      <Text as="div" tone="muted" size="sm">
        Liis Tamm - 2 KB
      </Text>
    </ListRow>
    <ListRow block>
      <strong>Yesterday at 18:20</strong>
      <Text as="div" tone="muted" size="sm">
        Claude for Liis - 1 KB
      </Text>
    </ListRow>
  </List>
)

export const FactList: Story = () => (
  <Facts>
    <Fact label="Type">Text file</Fact>
    <Fact label="Size">2 KB</Fact>
    <Fact label="Changed">2 h ago by Liis Tamm</Fact>
    <Fact label="A long label for a fact">A long value that wraps onto the next line when the panel is narrow</Fact>
  </Facts>
)

export const FolderPicker: Story = () => (
  <Stack>
    <Picker label="Folders">
      <PickerItem icon={<FolderIcon />} onClick={() => {}}>
        Clients
      </PickerItem>
      <PickerItem icon={<FolderIcon />} onClick={() => {}} disabled>
        Site photos (the folder itself)
      </PickerItem>
    </Picker>
    <Picker label="Empty folders">
      <PickerEmpty>No folders here.</PickerEmpty>
    </Picker>
  </Stack>
)

export const More: Story = () => (
  <Stack>
    <LoadMore state={{ data: { nextCursor: 'x' }, loadMore: () => {}, loadingMore: false, moreError: undefined }} />
    <LoadMore state={{ data: { nextCursor: 'x' }, loadMore: () => {}, loadingMore: true, moreError: undefined }} />
  </Stack>
)
