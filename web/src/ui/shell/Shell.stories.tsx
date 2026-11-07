import type { Story } from '@ladle/react'
import { useState } from 'react'
import { Button } from '../button/Button'
import { Field, Select } from '../field/Field'
import { Actions, Stack, Toolbar } from '../layout/Layout'
import { Text } from '../text/Text'
import {
  AppBody,
  AppMain,
  Brand,
  DropZone,
  FloatingPanel,
  MenuToggle,
  NavButton,
  NavItem,
  NavList,
  NavRail,
  SearchForm,
  SelectionBar,
  SkipLink,
  SplitView,
  TopBar,
  TopBarItem,
} from './Shell'

export default { title: 'App shell' }

export const Shell: Story = () => {
  const [open, setOpen] = useState(false)
  return (
    <>
      <SkipLink />
      <TopBar>
        <Brand to="/">Ruum</Brand>
        <TopBarItem grow>
          <Field label="Space" labelHidden>
            <Select defaultValue="p">
              <option value="p">Liis Tamm (personal)</option>
              <option value="t">Pärn Studio (admin)</option>
            </Select>
          </Field>
        </TopBarItem>
        <SearchForm id="s" label="Search by name" name="q" defaultValue="" onSubmit={(e) => e.preventDefault()} />
        <MenuToggle open={open} controls="nav" onToggle={() => setOpen(!open)} />
      </TopBar>
      <AppBody>
        <NavRail id="nav" label="Main" open={open}>
          <NavList>
            <NavItem to="/">Files</NavItem>
            <NavItem to="/shared">Shared with me</NavItem>
            <NavItem to="/trash">Trash</NavItem>
            <NavItem to="/agents">Agents</NavItem>
          </NavList>
          <NavList end>
            <NavItem to="/account">Liis Tamm</NavItem>
            <NavButton onClick={() => {}}>Sign out</NavButton>
          </NavList>
        </NavRail>
        <AppMain>
          <Toolbar>
            <h1>Files</h1>
            <Button variant="primary">Upload files</Button>
          </Toolbar>
          <Text tone="muted">The page goes here.</Text>
        </AppMain>
      </AppBody>
    </>
  )
}

export const Split: Story = () => (
  <SplitView
    asideLabel="Details"
    aside={
      <Stack>
        <h2>plan.txt</h2>
        <Text tone="muted">The details panel; a sheet on the phone.</Text>
      </Stack>
    }
  >
    <Text>The list.</Text>
  </SplitView>
)

export const Selection: Story = () => (
  <SelectionBar label="Selection">
    <span>3 items selected</span>
    <Button size="sm">Move</Button>
    <Button size="sm">Move to trash</Button>
    <Button variant="link">Clear selection</Button>
  </SelectionBar>
)

export const Drop: Story = () => (
  <Stack gap={5}>
    <DropZone active hint="Drop to upload to Pärna 12">
      <Text>The folder while files are dragged over it.</Text>
    </DropZone>
    <DropZone active={false} hint="">
      <Text>The folder at rest.</Text>
    </DropZone>
  </Stack>
)

export const Floating: Story = () => (
  <FloatingPanel label="Uploads" title="Uploading 2 of 3" open actions={<Button size="sm">Hide</Button>}>
    <Text truncate>a-very-long-file-name-of-a-photo-from-the-site.jpg</Text>
    <Actions align="start">
      <Button variant="link">Clear finished uploads</Button>
    </Actions>
  </FloatingPanel>
)
