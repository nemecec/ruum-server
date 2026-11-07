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
import { sample } from '../story'

export default { title: 'App shell' }

export const Shell: Story = () => {
  const [open, setOpen] = useState(false)
  return (
    <>
      <SkipLink />
      <TopBar>
        <Brand to="/">{sample('Ruum')}</Brand>
        <TopBarItem grow>
          <Field label={sample('Space')} labelHidden>
            <Select defaultValue="p">
              <option value="p">{sample('Liis Tamm (personal)')}</option>
              <option value="t">{sample('Pärn Studio (admin)')}</option>
            </Select>
          </Field>
        </TopBarItem>
        <SearchForm id="s" label={sample('Search by name')} name="q" defaultValue="" onSubmit={(e) => e.preventDefault()} />
        <MenuToggle open={open} controls="nav" onToggle={() => setOpen(!open)} />
      </TopBar>
      <AppBody>
        <NavRail id="nav" label={sample('Main')} open={open}>
          <NavList>
            <NavItem to="/">{sample('Files')}</NavItem>
            <NavItem to="/shared">{sample('Shared with me')}</NavItem>
            <NavItem to="/trash">{sample('Trash')}</NavItem>
            <NavItem to="/agents">{sample('Agents')}</NavItem>
          </NavList>
          <NavList end>
            <NavItem to="/account">{sample('Liis Tamm')}</NavItem>
            <NavButton onClick={() => {}}>{sample('Sign out')}</NavButton>
          </NavList>
        </NavRail>
        <AppMain>
          <Toolbar>
            <h1>{sample('Files')}</h1>
            <Button variant="primary">{sample('Upload files')}</Button>
          </Toolbar>
          <Text tone="muted">{sample('The page goes here.')}</Text>
        </AppMain>
      </AppBody>
    </>
  )
}

export const Split: Story = () => (
  <SplitView
    asideLabel={sample('Details')}
    aside={
      <Stack>
        <h2>{sample('plan.txt')}</h2>
        <Text tone="muted">{sample('The details panel; a sheet on the phone.')}</Text>
      </Stack>
    }
  >
    <Text>{sample('The list.')}</Text>
  </SplitView>
)

export const Selection: Story = () => (
  <SelectionBar label={sample('Selection')}>
    <span>{sample('3 items selected')}</span>
    <Button size="sm">{sample('Move')}</Button>
    <Button size="sm">{sample('Move to trash')}</Button>
    <Button variant="link">{sample('Clear selection')}</Button>
  </SelectionBar>
)

export const Drop: Story = () => (
  <Stack gap={5}>
    <DropZone active hint={sample('Drop to upload to Pärna 12')}>
      <Text>{sample('The folder while files are dragged over it.')}</Text>
    </DropZone>
    <DropZone active={false} hint="">
      <Text>{sample('The folder at rest.')}</Text>
    </DropZone>
  </Stack>
)

export const Floating: Story = () => (
  <FloatingPanel label={sample('Uploads')} title={sample('Uploading 2 of 3')} open actions={<Button size="sm">{sample('Hide')}</Button>}>
    <Text truncate>{sample('a-very-long-file-name-of-a-photo-from-the-site.jpg')}</Text>
    <Actions align="start">
      <Button variant="link">{sample('Clear finished uploads')}</Button>
    </Actions>
  </FloatingPanel>
)
