import { useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { tenantPath, useTenant } from '../../app/tenant'
import { CopyField } from '../../components/ui'
import { RegisterAgentDialog } from './AgentsPage'

/** The address of the MCP server: the public URL of Ruum and `/mcp` (STATUS.md, "MCP"). */
function mcpAddress(): string {
  return `${window.location.origin}/mcp`
}

/**
 * "Connect an agent" (card UX-03, journeys J5 steps 1-4): the MCP address with Copy and the steps
 * for the common apps. Apps that use MCP sign in with OAuth and need no key; the second card makes
 * an agent with its own key for scripts and the CLI.
 */
export function ConnectAgentPage() {
  const { tenant } = useTenant()
  const navigate = useNavigate()
  const [registering, setRegistering] = useState(false)
  const address = mcpAddress()
  return (
    <div className="page-body">
      <p>
        <Link to={tenantPath(tenant.id, 'agents')}>Agents</Link>
      </p>
      <h1>Connect an agent</h1>
      <p className="muted">
        Agents work in Ruum for you, with the access you give them. Every change they make shows their name and yours.
      </p>
      <div className="columns">
        <section className="card stack" aria-labelledby="connect-app">
          <h2 id="connect-app">Connect an AI app</h2>
          <p>Claude, ChatGPT and other apps that use MCP. Add this server address in the app:</p>
          <CopyField label="Server address" value={address} />
          <p className="muted small">
            The app opens Ruum in your browser. You choose where the agent may work and how long, then go back to the app.
          </p>
          <h3>Claude Desktop</h3>
          <ol className="steps">
            <li>Open Settings, then Connectors.</li>
            <li>Choose Add custom connector, and paste the server address.</li>
            <li>Choose Connect and allow the access in Ruum.</li>
          </ol>
          <h3>Claude Code</h3>
          <p>Run this in a terminal, then type /mcp in Claude Code and choose ruum to sign in:</p>
          <CopyField label="Command for Claude Code" value={`claude mcp add --transport http ruum ${address}`} />
          <h3>ChatGPT</h3>
          <ol className="steps">
            <li>Open Settings, then Connectors (developer mode must be on).</li>
            <li>Create a connector with the server address and OAuth.</li>
            <li>Allow the access in Ruum.</li>
          </ol>
        </section>
        <section className="card stack" aria-labelledby="connect-key">
          <h2 id="connect-key">Use a key in a script or the CLI</h2>
          <p>Create an agent with its own key. Give the key only to the program that needs it.</p>
          <CopyField label="Ruum address for the CLI" value={window.location.origin} />
          <div className="actions left">
            <button type="button" className="primary" onClick={() => setRegistering(true)}>
              Create an agent with a key
            </button>
          </div>
        </section>
      </div>
      {registering && (
        <RegisterAgentDialog
          onClose={() => setRegistering(false)}
          onCreated={(a) => navigate(tenantPath(tenant.id, 'agents', a.id), { state: { justCreated: true } })}
        />
      )}
    </div>
  )
}
