import { useState } from 'react'
import { Link, useNavigate } from 'react-router'
import { tenantPath, useTenant } from '../../app/tenant'
import { RegisterAgentDialog } from './AgentsPage'
import { Actions, Button, Card, Columns, CopyField, PageBody, ProseList, Text } from '../../ui'
import { t } from '../../i18n'

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
    <PageBody>
      <p>
        <Link to={tenantPath(tenant.id, 'agents')}>{t('shell.nav.agents')}</Link>
      </p>
      <h1>{t('files.welcome.connect')}</h1>
      <Text tone="muted">{t('agents.intro')}</Text>
      <Columns>
        <Card aria-labelledby="connect-app">
          <h2 id="connect-app">{t('connect.app.title')}</h2>
          <p>{t('connect.app.intro')}</p>
          <CopyField label={t('account.appPassword.server')} value={address} />
          <Text tone="muted" size="sm">
            {t('connect.app.flow')}
          </Text>
          <h3>{t('connect.claudeDesktop')}</h3>
          <ProseList ordered>
            <li>{t('connect.claudeDesktop.1')}</li>
            <li>{t('connect.claudeDesktop.2')}</li>
            <li>{t('connect.claudeDesktop.3')}</li>
          </ProseList>
          <h3>{t('connect.claudeCode')}</h3>
          <p>{t('connect.claudeCode.intro')}</p>
          <CopyField wrap label={t('connect.claudeCode.command')} value={`claude mcp add --transport http ruum ${address}`} />
          <h3>{t('connect.chatgpt')}</h3>
          <ProseList ordered>
            <li>{t('connect.chatgpt.1')}</li>
            <li>{t('connect.chatgpt.2')}</li>
            <li>{t('connect.chatgpt.3')}</li>
          </ProseList>
        </Card>
        <Card aria-labelledby="connect-key">
          <h2 id="connect-key">{t('connect.key.title')}</h2>
          <p>{t('connect.key.intro')}</p>
          <CopyField label={t('connect.key.address')} value={window.location.origin} />
          <Actions align="start">
            <Button variant="primary" onClick={() => setRegistering(true)}>
              {t('agents.register.title')}
            </Button>
          </Actions>
        </Card>
      </Columns>
      {registering && (
        <RegisterAgentDialog
          onClose={() => setRegistering(false)}
          onCreated={(a) => navigate(tenantPath(tenant.id, 'agents', a.id), { state: { justCreated: true } })}
        />
      )}
    </PageBody>
  )
}
