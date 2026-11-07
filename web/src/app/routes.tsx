import { Navigate, Outlet, type RouteObject } from 'react-router'
import { OfflineBanner } from '../components/OfflineBanner'
import { ToastProvider } from '../ui'
import { AcceptInvitationPage } from '../pages/AcceptInvitation'
import { AccountPage } from '../pages/Account'
import { AdminLayout, AuditPage, InvitationsPage, LimitsPage, MembersPage, PolicyPage } from '../pages/admin/AdminPages'
import { AgentPage } from '../pages/agents/AgentPage'
import { AgentsPage } from '../pages/agents/AgentsPage'
import { ConnectAgentPage } from '../pages/agents/ConnectAgentPage'
import { FilesPage } from '../pages/files/FilesPage'
import { ItemPage } from '../pages/files/ItemPage'
import { SearchPage } from '../pages/files/SearchPage'
import { SharedPage } from '../pages/files/SharedPage'
import { TrashPage } from '../pages/files/TrashPage'
import { NotFoundPage } from '../pages/NotFound'
import { OAuthConsentPage } from '../pages/OAuthConsent'
import { ShareLinkPage } from '../pages/share/ShareLinkPage'
import { SignInPage } from '../pages/SignIn'
import { RequireSession } from './session'
import { AccountRedirect, HomeRedirect, SectionRedirect, TenantLayout } from './Shell'

/** All routes of the UI. `createBrowserRouter` uses them in the app, `createMemoryRouter` in tests. */
export const routes: RouteObject[] = [
  {
    element: (
      <ToastProvider>
        <OfflineBanner />
        <Outlet />
      </ToastProvider>
    ),
    children: [
      { path: '/signin', element: <SignInPage /> },
      // A share link (`<public_url>/s#<token>`): no sign-in, the reduced shell.
      { path: '/s', element: <ShareLinkPage /> },
      {
        element: (
          <RequireSession>
            <Outlet />
          </RequireSession>
        ),
        children: [
          { index: true, element: <HomeRedirect /> },
          { path: 'account', element: <AccountRedirect /> },
          // The page links in the hints of `403 user_required` (decision 0003-7).
          { path: 'agents', element: <SectionRedirect to={['agents']} /> },
          { path: 'members', element: <SectionRedirect to={['admin', 'members']} /> },
          { path: 'audit', element: <SectionRedirect to={['admin', 'audit']} /> },
          { path: 'settings', element: <SectionRedirect to={['admin', 'policy']} /> },
          { path: 'oauth/consent', element: <OAuthConsentPage /> },
          { path: 'invitations/accept', element: <AcceptInvitationPage /> },
          {
            path: 't/:tenantId',
            element: <TenantLayout />,
            children: [
              { index: true, element: <Navigate to="files" replace /> },
              { path: 'files', element: <FilesPage /> },
              { path: 'files/:folderId', element: <FilesPage /> },
              { path: 'item/:nodeId', element: <ItemPage /> },
              { path: 'shared', element: <SharedPage /> },
              { path: 'trash', element: <TrashPage /> },
              { path: 'search', element: <SearchPage /> },
              { path: 'agents', element: <AgentsPage /> },
              { path: 'agents/connect', element: <ConnectAgentPage /> },
              { path: 'agents/:agentId', element: <AgentPage /> },
              { path: 'account', element: <AccountPage /> },
              {
                path: 'admin',
                element: <AdminLayout />,
                children: [
                  { index: true, element: <Navigate to="members" replace /> },
                  { path: 'members', element: <MembersPage /> },
                  { path: 'invitations', element: <InvitationsPage /> },
                  { path: 'policy', element: <PolicyPage /> },
                  { path: 'limits', element: <LimitsPage /> },
                  { path: 'audit', element: <AuditPage /> },
                ],
              },
              { path: '*', element: <NotFoundPage inShell /> },
            ],
          },
        ],
      },
      { path: '*', element: <NotFoundPage /> },
    ],
  },
]
