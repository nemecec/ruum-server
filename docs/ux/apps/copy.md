# UI copy of step 3

Status: `provisional`. The exact strings of the step 3 screens, in the voice of `../copy.md`
(informal and playful where nothing is at stake; calm and exact where something is: publishing,
access, secrets, privacy, reports). On acceptance, section 3.9 moves into `../copy.md`, the
hints into its section 4, the empty states into 5 and the emails into 6, and every string goes
into `web/src/locales/en.json` with its context (tone and length) in
`web/src/locales/context/en.json` (`../i18n.md`). *Italics* mark values that the UI fills in.

**Tone per area** (for the context file): `playful` for empty states, "ready" and "live"
successes and template cards; `neutral` for labels, steps, lists and filters; `calm` for the
publish dialog, requests, going back, access, secrets, privacy settings, every error, the report
page and the receipt page. The report and receipt pages are read by people who may never have
heard of Ruum: no Ruum jargon there at all (not even "space" or "item").

## 3.5 Agents: one change (APP-2)

| Where | Today | Step 3 |
|---|---|---|
| Consent title | *App* wants to connect to Ruum | *AI app* wants to connect to Ruum |
| Consent warning | Ruum can't check the name or address of this app. Continue only if you started this. | Ruum can't check the name or address of this AI app. Continue only if you started this. |
| Consent done | Connected. You can go back to *App*. | Connected. You can go back to *AI app*. |

## 3.9 Web apps (step 3)

Journeys J15 to J21 (`journeys.md`), screens A1 to A16 (`screens.md`).

### Create from a template (J15, A1, A2)

| Where | Copy |
|---|---|
| New menu | Folder / Database / Project from template... / Issue |
| Disabled, guest | Guests can't create projects in this space. An admin can turn this on in Sharing rules. |
| Dialog title | New project from template |
| Where line | In *Projects* [Change] |
| Groups | From Ruum / From *Studio Linn* |
| Card, web app | Web app with a database and sign-in: For apps where people sign in and keep data. |
| Card, static | Static site: Pages, a blog or a portfolio. No server needed. |
| Card, internal tool | Internal tool: Lists and forms for your team. |
| You get | You get: *an app, a Postgres database with sample data, 4 issue kinds*. |
| Card button | Choose |
| Second panel | Back / Project name / Placeholder: Project tracker / Where / Change |
| Placeholder field | Who is the *approver*? (optional) / You can do this later. |
| Access line | Everyone who can see *Projects* can see the new project. The app is open only to people you share it with. |
| Plan line | Plan: *2* of *3* apps. |
| Button | Create project |
| Progress title | Setting up *Project tracker* |
| Progress subtitle | From the template *Internal tool* *1.2.0* |
| Setup steps | Making folders / Setting up the database *tracker* / Adding sample data (masked) / Making the first preview of *Project tracker* / Adding issue kinds / Adding smart folders / Giving access |
| Step states | Waiting / Done / Skipped / Didn't work: *reason* |
| Leave note | You can leave this page. Ruum keeps going. |
| Done | *Project tracker* is ready to try! [Open the preview] [Go to the project] |
| Open placeholder | Nobody is the *approver* yet. [Choose people] |
| Setup failed | Part of the setup didn't work. What Ruum made is in *Project tracker*. [Try again] [Go to the project] |
| Next steps (Overview) | Next: try the preview, then publish it so people can use it. [Open preview] [Publish] |
| Made by agent | Made by *Agent* for *Person* |

### App page and status (J16, A3)

| Where | Copy |
|---|---|
| Tabs | Overview / Versions / Logs / Issues / Access / Settings |
| Page actions | Share / Publish / Open app |
| Badges | Not live yet / Live / Publishing / Needs attention / Paused / Didn't start |
| Facts | Live: Version *12*, published *2 hours ago* by *name* / Who can use: *8* people in *Studio Linn*, *2* guests / Approvers: *Kadri, Tõnu* (one line per app role with holders, at most 3 roles) / Right now: Asleep. It wakes on the next visit, in 1 to 3 seconds. / Right now: Awake, *1* instance. |
| Not live yet | *Project tracker* isn't live yet. Try a preview, then publish it. |
| Publishing | Ruum is publishing *Favourites list*. People keep using version *12* until it's done. |
| Status reasons (`degraded`) | `error_rate_high`: More requests fail than usual since *14:20*. [See issues] / `crash_loop`: The app keeps stopping. Ruum starts it again each time. [See logs] / `health_failing`: The app doesn't answer Ruum's health check. [See logs] / other: Something isn't right. Ruum keeps checking. |
| Paused reasons (`suspended`) | In the trash: *app* is in the trash. Nobody can use it. Restore it from the trash to use it again. / `over_limit`: *app* is paused: *Studio Linn* is over its plan's limit. An admin can change the plan. / `blocked_by_admin`: Ruum's team blocked *app*. Contact support to find out why. |
| Didn't start | Version *12* didn't start: *reason*. [See the steps] [Try again] |
| Traffic tiles | Visits / Errors / Speed / Up *12%* from yesterday / Down *3%* from yesterday / Same as yesterday / high |
| Tiles, no traffic | No visits in the last 24 hours. |
| Sections | Waiting for you / Last 24 hours / Issues / Previews |
| Secret missing | *STRIPE_KEY* has no value yet. The next publish waits for it. [Enter value] / (Can edit:) Ask *owner name* to enter it. |
| View only line | You can use *app*. People who can edit it see its logs and errors. |
| Offline status | Last checked *14:02* |
| Address | Address copied. / The address appears with the first preview. |
| Row links in Files | *1* request / *3* open issues / *2* previews |

### Steps of a preview or a publish (A5, 4.30)

| Stage | Step name (people) |
|---|---|
| `unpack` | Reading files |
| `validate` | Checking app.yaml |
| `bindings` | Connecting data |
| `migrations` | Updating tables |
| `image` | Packaging |
| `start` | Starting |
| `check` | Trying it in a browser |
| `switch` | Going live |
| `build`, `test` | (not shown in step 3) |

| Where | Copy |
|---|---|
| Badges | Getting ready: *step* / Ready to try / Live / Didn't start: *step* / Ended / Taken back |
| Ready | Ready to try. [Open preview] [Publish] |
| Check, passed | Ruum opened *5* pages in a browser: no errors. |
| Check, problems | Ruum opened *5* pages in a browser: *2* errors in the browser, *1* failed request, *1* server error. [Show] |
| Check, new issues | *2* new issues. [See issues] |
| Preview facts | Preview of *app* · by *name* · *3 min* ago / Ends in *6* days / Code: folder *app/* (*12* files, versions of *14:02*) / Code: *garden.tar.gz*, version of *14:02* / Draft: *favourites* |
| What changes heading | What changes when you publish |
| What it reaches heading | What it can reach |

**Step errors** (the people's copy of `AppStageError.hint`; "Details" holds the code, hint and
`error.details`):

| Hint or code | Copy for people |
|---|---|
| `validation_failed` with `pointer` | app.yaml isn't right at *bindings, db*: *message*. Fix it, then make a new preview. |
| archive too large (`payload_too_large`) | The code is larger than *50 MB*, the most your plan allows. Leave out what the app doesn't need, like node_modules. |
| `binding_not_allowed` | You can't give the app *Can change data* on *tracker*: you can only read it. Ask *owner name*, or give the app *Can read*. |
| a binding target not found (`not_found`) | app.yaml names *../garden*, which isn't there or which you can't see. |
| `database_branches_not_ready` | Previews with a database wait for a feature Ruum is still building. |
| `secret_missing` | *STRIPE_KEY* has no value yet. Enter it in Settings before you publish. |
| `preview_scopes_left_out` (warning) | The preview can't call *orders* of *Shop*: that API allows no calls from previews. The live app can. |
| `migration_failed` | The table change *0004_favourites* didn't work. Nothing changed. |
| start failed | The app didn't start. The last log lines say why. [See logs] |
| health failed | The app started but doesn't answer on *health path*. [See logs] |
| `slug_retired` | The address *garden* was used before and can't be used again. Pick another name. |
| `plan_limit_reached` | You've reached the app limit of your plan. Move an app you don't use to the trash, or an admin can change the plan. |
| other | This step didn't work. Ruum can't say more; the details below help your agent. |

### Publish, requests and going back (J18, A4, A6)

| Where | Copy |
|---|---|
| Publish title | Publish *Favourites list*? |
| Request title | Publish request from *Agent* |
| Request by-line | *Agent* for *Person* asked *3 minutes* ago. Ends in *23 h 57 min*. |
| Open preview | Open preview |
| Changes heading | What changes |
| Access changes | New access: *garden*, can change data / No longer reaches *Photos/Garden* / Can call *orders* of *Shop*: *orders.read* |
| Role changes | New app role: *approver* / Removes the app role *approver*. Its *2* holders keep the grant, and it works again if a version brings the role back. |
| Table changes | Adds the table *favourites* with *3* columns, in *garden*. / Changes *2* tables in *garden*. [Show the change (SQL)] |
| Log field changes | *2* new log fields. |
| Nothing changes | Nothing changes in access, roles or tables. Only the code is new. |
| Fixes line | Fixes: *TypeError: plants is undefined* |
| Classes headings | Who can see the new columns / Who can see the new log fields |
| Effect | It becomes version *13*. People using *app* get it at once. You can go back to version *12* at any time. |
| Effect, migration | Ruum records a restore point of *garden* first. If you go back to version *12* later, the table changes stay. |
| First publish, access | Who can use it: people you share it with. [Share after publishing] |
| First publish, privacy | Browser errors: Privacy mode (recommended): errors and speed only, no IP addresses, people as stand-ins. / Full mode: also what people clicked and the requests that failed. Tell your users in your privacy notice. [Copy the privacy notice paragraph] |
| Buttons | Cancel / Publish / Deny / Approve / Approve and publish / Approve and publish, confirm *4* items |
| Deny reason | Tell *Agent* why (optional) |
| Toasts | *app* is live! Version *13*. [Open app] / Approved. *Agent* can publish it in the next 15 minutes. / Denied. *Agent* was told. |
| Publish failed | Publishing didn't work at *Updating tables*. Nothing changed; version *12* is still live. [See the steps] |
| Request states | Waiting / Approved / Denied / Ended / Used |
| Ended | This request has ended. *Agent* can ask again. |
| Decided by another | *Name* already approved this at *14:05*. / *Name* already denied this at *14:05*. |
| Cannot decide | Only *Person* or people who can manage *app* can decide this. |
| Cannot publish (migration) | This change alters *garden*, which only people who can manage it may approve. Ask *owner name*. |
| Stale | The preview changed after *Agent* asked. Ask it for a new request. |
| Check warning | The browser check found *2* errors. [Show] |
| Go back title | Go back to version *11*? |
| Go back body | Version *11* becomes live again. Version *13* stays in Versions; you can publish it again later. |
| Go back body, migration | The tables stay as version *13* changed them: Ruum doesn't undo table changes. Version *11* must work with them. |
| Go back buttons | Go back / Go back anyway |
| Go back toast | Version *11* is live again. [Undo] |
| Auto rollback banner | Ruum went back to version *13* at *14:20*: errors went up after version *14*. [See issues] [Publish version 14 again] |
| Version rows | Version *12* / Published *14:20* by *name* / (went back by *name*) / (went back by Ruum) / Ruum went back at *14:20*. |
| Sections | Previews / Live versions / Ended previews |
| Preview row menu | Copy address / Compare with live |

### Access (J17, A9, A10)

| Where | Copy |
|---|---|
| Intro | What *app* can reach. People who use the app don't get this access; the app does. |
| Sections | Databases / Files and folders / Other apps |
| Rows | *tracker*: can change data / *Projects/Kalamaja*: can read / *Invoices* (API *orders*): can call *orders.read* / given by *name*, *10 Nov* / an item you can't see |
| Preview note | Previews use a masked copy of each database. |
| Preview changes note | The preview *Favourites list* changes this. [See the preview] |
| Button | Give access |
| Dialog title | Give *app* access |
| Kinds | A database / A file or folder (read only) / Another app's API |
| Access choices | Can read / Can change data |
| Allowed line | You can manage *tracker*, so you can give this. |
| Capped line | You can only read *tracker*, so the app can get at most Can read. |
| Name field | Name in the code / The app reads it from RUUM_BINDING_*TRACKER*. |
| Name taken | The app already uses the name *tracker*. Pick another name. |
| Summary | Ruum adds these lines to app.yaml and makes a preview. The live app doesn't change until you publish. |
| Masked line | The preview uses a masked copy of *tracker*: personal values are stand-ins. |
| Archive source | This app comes from an archive, so Ruum can't change its app.yaml. Add these lines, or ask your agent to: [Copy lines] |
| Buttons | Make a preview / Copy lines / Close |
| Toast | Made a preview with access to *tracker*. |
| Remove title | Remove *app*'s access to *tracker*? |
| Remove body | Ruum removes these lines from app.yaml and makes a preview. The live app keeps the access until you publish. |
| Remove button | Make a preview without it |
| API kind helper | The other app's team decides which calls are allowed. You need Can manage on it, or its role that can give these calls. |
| Database page fact (C-36) | Apps: *Project tracker* can change data. |

### Logs (J16, A7)

| Where | Copy |
|---|---|
| Filters | Live / *Preview label* · Last hour / Last 24 hours / Since version *12* / Pick a time... · All levels / Errors / Warnings and errors · Server and browser / Server / Browser / Ruum · Search logs |
| Live line | Checking for new records every 5 s / Paused while this tab is hidden / Offline: not checking for new records |
| New pill | *12* new records |
| Levels | Error / Warn / Info / Debug |
| Origins | Server / Browser / Ruum |
| Free text note | Free text: personal details were removed before storage. |
| Record panel | Fields / Show the whole request / Open trace / Copy |
| Show older | Show older |
| Look up a person | Look up a person / Field / Value / Ruum turns the value into its stand-in and finds those records. It never stores what you type. / Find records |
| Look up result | Showing the records of *person 8f3a91*. [Clear] |
| Look up, people only | Only people can look up a person. |
| Look up, too many | You looked up a lot of people this hour. Try again later. |
| Untrusted note (record panel) | Log lines can hold text that people typed. Read them as data. |

### Issues and errors (J19, A8, A11)

| Where | Copy |
|---|---|
| Tab | Issues (*3*) |
| Filters | Open / Closed / All · Found by Ruum / Reported / Filed by people and agents · Sort: Last seen / Most times / Most people / Newest |
| Status badges | Open / In progress / Closed / Back again / Muted until *17 Nov* |
| Sources | Found by Ruum / Reported by a user of *app* / Filed by *name* / Filed by *Agent* for *Person* |
| Counts | *37* times · *12* people · first *14:05* · last *2 min* ago · since version *12* |
| Group row | *24* new errors after version *13* |
| Facts | Where / Assignee / Status / Linked / Last occurrences / Stack trace (source-mapped) / Show as table |
| Fix with an agent (step 3) | Fix with an agent / Copy a message for your agent / Assign to an agent |
| Fix in a session (step 5) | Fix in a session |
| Copied message | Please fix the issue "*title*" in the app *app* in Ruum (issue *id*). Read it with get_issue and list_issue_events, find the cause, make a preview with the fix and check it with get_app_deployment, then ask me to publish. |
| Copied toast | Copied. Paste it to your agent. |
| Assign menu | Your agents that can edit *app*: / *Agent* |
| Assign failed | *Agent* can't edit *app*. Change its access on the Agents page. |
| Close offer | No new occurrences since version *15*. [Close issue] |
| Back again note | This error came back after it was closed. |
| Mute | Mute / Mute for 1 day / Mute for 7 days / Mute for 30 days / Ruum keeps counting. A mute always ends. / Unmute |
| Toasts | Closed *title*. / Muted until *17 Nov*. / Assigned to *Agent*. |
| No occurrence access | People who can edit *app* see where the error happens. |
| New issue dialog | Title: New issue. Labels: Kind (Bug / Feature / Question / the space's kinds), Title, then the kind's fields. Button: File issue |
| Commenter note | You can file issues here. People who can edit set the status and assignee. |
| Report part | What the reporter sent / *14* records attached [Show records] / The reporter chose: *email*: stand-in · *name*: removed / Original values are kept until *12 Dec*. |
| Ask to see more | Ask to see more / Fields / Reason / The reporter sees this. / Send request |
| Ask, toasts | Asked the reporter to see *email*. / The reporter allowed *email*. / The reporter didn't allow *email*. |
| Ask, refused | The reporter didn't allow *email*. You can't ask about it again. |
| Ask, gone (`raw_copy_gone`) | The original values of this visit are deleted, so the reporter can't share them any more. |
| Ask, agents | (agents only; never in the UI) |

### Sharing and app roles (J20, A12)

| Where | Copy |
|---|---|
| Role menu groups | In Ruum / In the app |
| Ruum roles on an app | Can use: open and use the app / Can edit: also make previews, publish, see logs and errors / Can manage: also settings, secrets and sharing |
| App role option | *approver*: *Can approve hours* (from the app) |
| Default helper | Everyone who can use *app* is a *member*: *Can log hours*. |
| No app roles | This app has no roles of its own. |
| No live version | App roles come from the live version. Publish first. |
| Role includes use | An app role includes using the app. |
| Guest line | *Helena* is a guest in *Studio Linn*. She can use the app; she sees nothing else here. |
| List row | Can use · *approver* · *admin* |
| Row menu | Remove *approver* / Change access / Remove access |
| Toasts | *Kadri* and *Tõnu* are now *approver* in *app*. / Removed *approver* for *Kadri*. [Undo] |
| Inactive role | Not in the live version. Grants stay, and work again if a version brings the role back. |
| Delegated intro | You can give app roles because you are an *admin* of *app*. |

### Settings (A15)

| Where | Copy |
|---|---|
| Sections | Who can use it / Secrets / Errors and privacy / "Report a problem" button / Go back by itself / Address / API keys |
| Who can use it | People you share it with (they sign in) / Anyone on the internet |
| Public, not approved | Ruum's team must turn this on for *Studio Linn* first. Ask Ruum's support. |
| Public, not owner | Only owners and admins of *Studio Linn* can open an app to the internet. |
| Public, confirm | Title: Open *app* to everyone on the internet? Body: Anyone with the address can use it, without signing in. The "Report a problem" button stays off until you turn it on. Button: Open to everyone |
| Secret kinds | Made by Ruum / Entered by a person |
| Secret states | No value yet / Set by *name* on *10 Nov* / Rotated *3 Nov* · every *90* days / Previews get this value |
| Secret buttons | Enter value / Change value / Rotate now |
| Enter dialog | Title: Enter *STRIPE_KEY*. Label: Value. Toggle: Show while typing. Switch: Previews get this value / Turn on only for test keys. Previews run code that isn't published yet. Note: Ruum never shows this value again, also not to you. Button: Save value |
| Secret toasts | Saved *STRIPE_KEY*. The app gets it at its next start. / Rotated *SESSION_KEY*. The old value works for *24 hours* more. |
| Rotate confirm | Title: Rotate *SESSION_KEY* now? Body: Ruum makes a new value. The old one keeps working for *24 hours*, then stops. Button: Rotate now |
| Secrets, agent note | Agents see which secrets are missing, never their values. |
| Privacy mode | Browser errors: Privacy mode (recommended) / Full mode |
| Privacy mode help | Privacy mode records errors and speed only: no IP addresses, no form values, people as stand-ins. |
| Full mode help | Full mode also records what people clicked and the requests that failed, for *7* days. Tell your users in your privacy notice. |
| Raw copy | Keep original personal values, encrypted, for *7* days / Off: Ruum stores only stand-ins. On: a reporter can choose to share their own values with you. Each person's values have their own key, so deleting a person deletes their values. |
| Raw copy, people only | Only people can turn this on or make it longer. |
| Log fields | Log fields: *2* not confirmed. They count as Personal. [Review log fields] |
| Privacy notice | Copy the privacy notice paragraph / Copied. Paste it into your app's privacy notice. |
| Privacy notice paragraph (privacy mode) | This app runs on Ruum. When something goes wrong, Ruum records the error, the page and the browser, without your IP address. You appear only under a code that Ruum can't turn back into your name. Records are deleted after *7* days. |
| Widget | Show it in the live app / Place: Bottom right / Bottom left / Top right / Top left |
| Widget off, public | Off for apps on the internet until you turn it on. |
| Auto rollback | Go back to the previous version when errors go up after a publish / When errors are at least *3* times higher than before, and above 1%. |
| Address | Your own domain / Add domain / Waiting for DNS / Getting a certificate / Active / Didn't work: *reason* / Add these DNS records at your domain's provider: / Remove domain |
| Domain, not approved | Ruum's team must turn this on for *Studio Linn* first. |
| API keys | New API key / Name (who uses it) / API / Calls it may make / Requests per minute, at most / Ends in (days) / Copy the key now. Ruum shows it only once. [Copy] |
| Revoke key confirm | Title: Turn off the API key for *partner*? Body: Programs that use it stop working at once. Button: Turn off key |
| Settings saved | Saved. |

### Address of a personal space's apps (S-35, decision 0008-1)

Space settings, Sharing rules tab, personal spaces only (`Tenant.app_slug`, `updateTenant`
with `app_slug`). The address part is public and permanent, so the copy is calm and exact. The
catalogue keys are `appAddress.*` (`web/src/locales/en.json`, Estonian in `et.json`).

| Where | Copy |
|---|---|
| Heading | Address of your apps |
| Fixed | Every app in this space has **ws-7k2m9q** in its address. Anyone can see it, and it never changes. |
| Not fixed yet | Your first app gives this space a random address, like ws-7k2m9q. It says nothing about you. Prefer a name you can read? Choose it now. |
| Field | Label: Choose an address. Helper: 3 to 40 characters: lower-case letters, digits and single dashes. Anyone can see it, and you can't change it later. Button: Use this address |
| Confirm | Title: Use *linn-apps* for good? Body: This address is public: it appears in DNS and in public certificate logs, so anyone can find it. You can't change it later, and nobody can use it again, even after you delete this space. Button: Use *linn-apps* |
| Saved (toast) | This space now has *linn-apps* in its app addresses. |
| `conflict` + `app_slug_taken` | *linn-apps* is taken. Try another one. |
| `conflict` + `app_slug_fixed` | This space already has an address for its apps. It can't change. |
| `validation_failed` + `/app_slug` | Use 3 to 40 lower-case letters, digits and single dashes, with no dash at either end. Some words, like www or api, are kept for Ruum. |

### Report page (J21, A13)

Calm tone. No Ruum words beyond the name "Ruum".

| Where | Copy |
|---|---|
| Widget button | Report a problem |
| Error prompt | Something went wrong. Tell the team what you were doing? [Report a problem] [Not now] |
| Title | Report a problem with *app* |
| Subtitle | *app* runs on Ruum. Your report goes to the people who made it. / (with C-35) *app* runs on Ruum. Your report goes to *Studio Linn*, who made it. |
| Part 1 | What happened / What were you doing, and what went wrong? / Placeholder: I saved my hours for Monday and the page went blank. |
| Part 1, expected (C-34) | What did you expect? (optional) |
| Part 1, empty | Tell the team what happened. |
| Part 2 | What Ruum attaches / You see everything before it is sent. Remove what you don't want to share. |
| Screenshot | Screenshot / Draw or blur / Remove / Screenshot removed. [Put it back] / Add a screenshot (without C-28: Paste an image, choose a file, or capture this tab.) |
| Details | Details / Page */hours* · *Firefox 133* · *1280 × 800* · version *7* |
| Records | Records of this visit / Ruum found *14* records of your visit. *3* of them hold personal details. / Ruum found *14* records of your visit. None of them hold personal details. / Ruum found no records of this visit. |
| Records, for all | Do this for all: |
| Choices | Share as it is / Use a stand-in / Remove |
| Choice help | Share as it is: the team sees your value. / Use a stand-in: the team sees a code instead, the same code each time. / Remove: the value is gone for good. |
| Not kept | (not kept) / *app* doesn't keep original values, so Ruum can only use a stand-in or remove it. |
| Own visit | Only records of your own visit. Nobody else's details are ever attached. |
| Drawing layer | Draw or blur / Pen / Box / Arrow / Text / Blur / Undo / Skip drawing / Done |
| Part 3 | Send / Email for an answer (optional) / Placeholder: name@example.com / Only the team sees it. Ruum deletes it 30 days after the report is closed. |
| Button | Send report / Sending... |
| Done, member | Thanks, your report is in. The team sees it in Ruum. [Open the report] [Close this window] |
| Done, visitor | Thanks, your report is in. Keep this link to follow it. Ruum shows it only once. [Copy link] [Close this window] |
| Link copied | Link copied. |
| Session ended | This report page has ended. Go back to *app* and click Report a problem again. |
| Too many | That was a lot of reports at once. Try again in a few minutes. |
| Screenshot too large | The screenshot is larger than 5 MB. Make the window smaller, or leave it out. |
| Offline | You're offline. Your text stays here. |

### Receipt page (J21, A14)

| Where | Copy |
|---|---|
| Title | Your report: *title* |
| Subtitle | To the team of *app* · sent *10 Nov, 14:25* |
| Status | Status / Received / Being worked on / Closed |
| Closed note | The team closed your report. Thanks for telling them. |
| Requests heading | The team asks |
| Request | *app*'s team asks to see: *email*. / Their reason (written by the team): *reason* |
| Request buttons | Allow / Don't allow |
| After allow | You allowed *email*. The team can see it now. |
| After don't allow | The team can't ask about *email* again. You can still allow it here later. |
| Shared heading | What you shared |
| Shared line | *Screenshot, details, 14 records* / *email*: stand-in · *name*: removed / [Change] |
| Change window | You can change your choices until *12 Dec*. Then Ruum deletes the original values. |
| Values gone | Ruum deleted the original values of your visit. Stand-ins and removed values stay as they were. |
| Link not working | This link doesn't work. The report may have been removed. |

### Pages on the app's address (A16)

| Page | Copy |
|---|---|
| Preview bar | Preview of *app*. Only people who can edit it see this. Data: a masked copy. [Back to Ruum] |
| Sign in | Sign in to use *app* / [Sign in with Ruum] |
| No access | You don't have access to *app*. Ask the person who shared it with you. [Go to Ruum] |
| Waking up | Waking up *app*... |
| Not answering | *app* isn't answering right now. Try again in a minute. [Try again] [Report a problem] |
| Paused | *app* is paused. / *app* is paused for now. / *app* is blocked. / If you use it, tell the person who shared it with you. |
| Preview ended | This preview has ended. A newer one may exist; look in Ruum. [Go to Ruum] |
| Not found | There's no app at this address. Check the address, or ask the person who sent it. |

## 4. Errors: the people's copy of the step 3 hints

Step 3 adds no error code (decision 0006-9). The UI maps code and hint together; where a hint is
not listed, the code's copy of `../copy.md` section 4 applies.

| Code + hint | Copy for people | Action |
|---|---|---|
| `permission_denied` + `binding_not_allowed` | You can't give the app *Can change data* on *tracker*: you can only read it. Ask *owner name*, or give the app *Can read*. | Under the role choice |
| `permission_denied` + `public_apps_not_allowed` | Ruum's team must turn on apps for everyone on the internet for *Studio Linn* first. Ask Ruum's support. | Close |
| `permission_denied` + `custom_domains_not_allowed` | Ruum's team must turn on your own domains for *Studio Linn* first. Ask Ruum's support. | Close |
| `permission_denied` + `tenant_admin_required` | Only owners and admins of *Studio Linn* can do this. | Close |
| `permission_denied` + `publishAppDeployment` | (agents only; never shown to people) | |
| `conflict` + `secret_missing` | *STRIPE_KEY* has no value yet. Enter it before you publish. | Enter value / Ask *owner name* |
| `conflict` + `raw_copy_gone` | The original values of this visit are deleted, so the reporter can't share them any more. | Close |
| `conflict` + `database_branches_not_ready` | Previews with a database wait for a feature Ruum is still building. | Close |
| `conflict` + `approval_stale` | The preview changed after *Agent* asked. Ask it for a new request. | Close |
| `conflict` + `slug_retired` | The address *garden* was used before and can't be used again. Pick another name. | Under the field |
| `conflict` (refused field asked again) | The reporter didn't allow *email*. You can't ask about it again. | Close |
| `user_required` | (agents only; never shown to people) | |
| `validation_failed` on `app.yaml` | app.yaml isn't right at *bindings, db*: *message*. Fix it, then make a new preview. | Details |
| `rate_limited` on a pseudonym lookup | You looked up a lot of people this hour. Try again later. | Retry later |

## 5. Empty states

| Place | Title | Text | Action |
|---|---|---|---|
| Templates from the space | (group hidden) | | |
| App, not live yet | *app* isn't live yet | Try a preview, then publish it so people can use it. | Open preview / Publish |
| Previews | No previews right now | Your agent makes one with every change, or you can make one from Access. | |
| Live versions | Nothing published yet | Each publish becomes a version here, so you can always go back. | |
| Waiting for you | (section hidden) | | |
| Issues on the Overview | No new issues in the last 7 days | Quiet is good. Errors that Ruum finds and reports from users show up here. | |
| Issues tab | No issues | Ruum checks every visit. Errors it finds, and reports from people who use the app, show up here. | |
| Issues, filtered | No issues match these filters | | Clear filters |
| Logs | No log records in this time | Try a longer time, or check that the app writes to standard output. | Last 24 hours |
| Logs, filtered | No records match these filters | | Clear filters |
| Logs, look up | No records about this person | Ruum found no records with *person 8f3a91* in this time. | Clear |
| Access | *app* reaches nothing yet | It can't read your files, databases or other apps until you give it access. | Give access |
| Database picker | No databases here yet | Make one in a folder first. | New database |
| Secrets | No secrets | This app needs none. Databases need no password: the app reaches them through its access. | |
| API keys | No API keys yet | Partners use them to call this app's API. | New API key |
| Your own domain | Just the Ruum address for now | | Add domain (when approved) |
| App roles in the share dialog | This app has no roles of its own | | |
| Issues of a folder | No issues yet | When someone files one, or Ruum finds an error, it lands here. | New issue |
| Last occurrences | No occurrences in the last 7 days | | |
| Receipt, no requests | (section hidden) | | |

## 6. Emails

| Email | Subject | Body (plain text) |
|---|---|---|
| Report received (to a reporter who left an email) | Your report about "*app*" is in | Thanks for telling the team of "*app*" about a problem. / You can follow your report here: *receipt link* / Only you have this link. Keep it to yourself. / Ruum deletes your email address 30 days after the report is closed. / If you didn't send a report, you can ignore this email. |
| The team asks to see more | The team of "*app*" asks about your report | The team of "*app*" asks to see some details of your report: *email*. / Their reason (written by the team): "*reason*" / Decide on your report's page: *receipt link* / You can say no. Nothing is shared until you allow it. / If you didn't send a report, you can ignore this email. |
| Report closed | Your report about "*app*" is closed | The team of "*app*" closed your report "*title*". / See it here: *receipt link* / Ruum deletes your email address in 30 days. |

The emails go only to reporters who typed an email (design 9.7). The receipt link is the only
link in them; the text says who sent the request in words ("the team of *app*") and never names a
person. The HTML part says the same, with the link as "Open your report".
