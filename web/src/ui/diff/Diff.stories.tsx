import type { Story } from '@ladle/react'
import { DiffChange, DiffList, DiffView } from './Diff'

export default { title: 'Diff' }

export const Changes: Story = () => (
  <DiffView>
    <p>3 changes</p>
    <DiffList>
      <DiffChange kind="Changed" where="Line 2" old="Rosa canina" new="Rosa rugosa" />
      <DiffChange kind="Added" where="Line 7" new="A new line" />
      <DiffChange kind="Removed" where="Line 9" old="An old line" />
      <DiffChange kind="Changed" where="plants > 3">
        <DiffList>
          <DiffChange kind="Format" format="indent changed" />
        </DiffList>
      </DiffChange>
    </DiffList>
  </DiffView>
)

export const Same: Story = () => (
  <DiffView>
    <p>These versions are the same.</p>
  </DiffView>
)

export const Busy: Story = () => (
  <DiffView busy>
    <p>Comparing...</p>
  </DiffView>
)
