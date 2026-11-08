# Glossary for translators

Status: `provisional` (copywriter, 2026-11-08). The approved English terms of the web UI, the
emails and the error texts, and the words that stay as they are in every language. Translators
use it with the context of each message (`web/src/locales/context/en.json`) and the voice
(`copy.md`, section 1). The terms come from `ia.md` (one word per concept); this page adds what a
translator needs. Each language adds its own column of approved translations here, or in
Weblate's glossary once it runs (design 12.4.1).

## How to use it

- **One term, one translation.** Translate each term the same way on every screen. If a screen
  seems to need another word, ask first: it is probably another concept.
- **"Never" words.** The words in the last column are wrong in the UI, also in translation (do not
  translate "item" as the local word for "object" or "node").
- **Case and inflection.** A term inside a sentence has its own message where English needs
  another case (`role.edit` "Can edit", `role.edit.inline` "can edit"). Inflect freely inside one
  message; never change the case of a placeholder value in code.
- **Tone.** The context file gives each message `playful`, `neutral` or `calm`. Keep the tone, not
  the words: a playful line may use a local idiom; a calm line stays plain and exact.
- **Address.** Informal "you" in every language (Estonian "sina", German "du", French "tu").

## Approved terms

### Spaces and people

| Term | Meaning | Never |
|---|---|---|
| space | Where your files live: your **personal space**, or a **team space** you share | tenant, organisation, workspace, account |
| personal space | The space every person gets at the first sign-in | |
| team space | A space for a group of people | |
| member | A person in a team space, with a member role | user (in running text) |
| Owner / Admin / Member / Guest | The member roles of a space (`memberRole.*`) | Viewer, Editor (for member roles) |
| guest | A person outside the space who sees only what is shared with them | external user |
| invitation, invite | How people join a space | |
| Space settings | The admin pages of a space | Admin console, preferences |
| Sharing rules | The space-wide switches for links, guests and sharing | policy |
| Plan | What the space pays for and its limits | quota, limits (alone), subscription |
| Activity log | The record of events in a space (admins) | audit log, journal |

### Files

| Term | Meaning | Never |
|---|---|---|
| item | A file or folder, when the kind does not matter | node, object, entry, resource |
| file / folder | | document (for any file), directory |
| Files | The top of the tree, and the page | root, home, drive |
| tag | A word you attach to items | label, category |
| version | One saved state of a file | revision, snapshot, commit |
| current version | The newest version | head, latest |
| version name; Name this version | A named version is kept forever | label, pin |
| Restore | Bring back: a version, or an item from the trash | revert, roll back, undelete, recover |
| Compare; Compare as | Show the changes between versions | diff |
| Trash; Move to trash | Where deleted items wait | bin, recycle bin |
| Delete for good; Empty trash | The only actions that delete. Calm, exact, never playful | purge, delete forever |
| upload | | transfer |
| Shared with me | The page of items others shared with you | |

### Sharing and access

| Term | Meaning | Never |
|---|---|---|
| access; People with access | What a person or agent may do with an item | permission, grant, ACL |
| Give access / Remove access / Change access | | grant, revoke |
| Can view / Can comment / Can edit / Can manage | The roles on an item (`role.*`) | Viewer, Editor, Owner (on items) |
| share | Give people access to an item | |
| link; Anyone with the link | A share link | URL token, secret URL |
| Turn off (a link, a key, an app password) | Stop it working | revoke, invalidate, disable |
| Everyone in *Space* | All members of a space | |
| from folder *Name* | Access that comes from a folder above | inherited |

### Agents

| Term | Meaning | Never |
|---|---|---|
| agent | An AI or program that works in Ruum for a person | bot, assistant, AI (alone), integration |
| *Agent* for *Person* | The by-line of every agent | |
| app | A program that asks to connect (OAuth consent screen) | client |
| Connect / Allow / Don't allow | The consent actions | authorize |
| key | An agent key for scripts and the CLI | token, API key (alone) |
| app password | A password for WebDAV clients (Finder, Explorer) | |
| Pause / Resume | Stop and start an agent | disable, enable |
| Undo changes | Undo an agent's run of changes | revert, rollback |
| run | Changes an agent made close together | batch, session |
| Where / Highest role / Ends on / Changes per hour | The parts of an agent's access | scope, max role, budget |
| signed-in device | A session on the Account page | session (as a noun on screen) |
| sign-in method | Google, Microsoft or a passkey | identity, IdP |

### Databases (step 2, `ia.md`)

| Term | Meaning | Never |
|---|---|---|
| database | A database item in Files | db, instance |
| query; Read only / Can change data | | statement, SQL console, write mode |
| migration | A change to the tables | schema change (alone), DDL |
| branch | A copy of a database to try changes | fork, clone, sandbox |
| restore point | A moment you can go back to | snapshot, checkpoint |
| privacy class: Normal / Personal / Secret | What agents see of a column | data class, sensitivity |
| stand-in | The value agents see instead of personal data | pseudonym, hash, token |
| hidden | A secret value agents never see | masked, redacted |
| write access / personal data access | Short-time extra access for agents | write grant |
| request; Waiting for you; Approve / Deny | An agent asks a person | ticket |

## Do not translate

Keep these as they are, in every language, also inside sentences:

| Kind | Words |
|---|---|
| Product name | **Ruum** (the name; it is not the Estonian word "ruum" in running text: see `i18n.md`, "Estonian") |
| Other products | Google, Microsoft, Claude, Claude Desktop, Claude Code, ChatGPT, Finder, Explorer, Firefox, Safari, Chrome, macOS, Windows, iPhone |
| Protocols and formats | MCP, OAuth, WebDAV, CLI, API, JSON, XML, CSV, PDF, HTML, Markdown, SQL, UUID |
| Database names and words | Postgres, SQLite, SQL keywords (SELECT, WHERE, LIMIT, CREATE TABLE), extension names (postgis, vector, pg_trgm, ...) |
| Typed text | Commands and what people type: `/mcp`, `ruum`, server addresses |
| Codes | Error codes (`not_found`), request IDs, key prefixes (`ruum_agent_...`) |
| Placeholders | Every `{name}` in a message, and tags such as `<b>`, `<link>`, `<button>`: keep them, move them where the grammar needs them |
| Plan names | Free, Personal, Team, Business: keep unless marketing gives a local name |

Translate the names of other products' screens and buttons (ChatGPT's "Settings", Claude's
"Connectors") with the words those products show in your language, not literally.

## Style per language

Each language gets its own section here (formality, punctuation, numbers and dates,
gender-neutral wording). Estonian: `i18n.md`, "Estonian".
