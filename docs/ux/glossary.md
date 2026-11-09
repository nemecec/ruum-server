# Glossary for translators

Status: `provisional` (copywriter, 2026-11-08). The approved English terms of the web UI, the
emails and the error texts, and the words that stay as they are in every language. Translators
use it with the context of each message (`web/src/locales/context/en.json`) and the voice
(`copy.md`, section 1). The terms come from `ia.md` (one word per concept); this page adds what a
translator needs. Each language adds its own column of approved translations here (Estonian is
the first), or in Weblate's glossary once it runs (design 12.4.1).

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

The column "Estonian" holds the approved Estonian translation (`style-guides/et.md`). The words in
parentheses that end in "-" are the stems that every form of the term starts with, or contains
(Estonian inflects and compounds: "kaust", "kausta", "kaustas", "alamkaust"). Where a row has
stems, `web/scripts/check-translations.mjs` checks each message: when the English text uses the
term (or its plural), the Estonian text must contain one of the stems. Alternatives in a cell are
separated by " / " or "; " and pair by position; one Estonian entry stands for all of them.

### Spaces and people

| Term | Meaning | Never | Estonian |
|---|---|---|---|
| space | Where your files live: your **personal space**, or a **team space** you share | tenant, organisation, workspace, account | ruum (ruum-) |
| personal space | The space every person gets at the first sign-in | | isiklik ruum (isikli-) |
| team space | A space for a group of people | | meeskonnaruum (meeskonnaruum-) |
| member | A person in a team space, with a member role | user (in running text) | liige (liige-, liikme-) |
| Owner / Admin / Member / Guest | The member roles of a space (`memberRole.*`) | Viewer, Editor (for member roles) | Omanik (omanik-) / Administraator (administraator-) / Liige (liige-, liikme-) / Külaline (külali-) |
| guest | A person outside the space who sees only what is shared with them | external user | külaline (külali-) |
| invitation, invite | How people join a space | | kutse, kutsuma (kutse-, kutsu-) |
| Space settings | The admin pages of a space | Admin console, preferences | Selle ruumi seaded |
| Sharing rules | The space-wide switches for links, guests and sharing | policy | Jagamise reeglid (reegl-, reegel-) |
| Plan | What the space pays for and its limits | quota, limits (alone), subscription | pakett (paket-) |
| Activity log | The record of events in a space (admins) | audit log, journal | Tegevuslogi (tegevuslogi-) |

### Files

| Term | Meaning | Never | Estonian |
|---|---|---|---|
| item | A file or folder, when the kind does not matter | node, object, entry, resource | üksus (üksus-) |
| file / folder | | document (for any file), directory | fail (fail-) / kaust (kaust-) |
| Files | The top of the tree, and the page | root, home, drive | Failid (fail-) |
| tag | A word you attach to items | label, category | silt (silt-, sildi-, silte-) |
| version | One saved state of a file | revision, snapshot, commit | versioon (versioon-) |
| current version | The newest version | head, latest | praegune versioon (praegu-) |
| version name; Name this version | A named version is kept forever | label, pin | versiooni nimi; Anna versioonile nimi |
| Restore | Bring back: a version, or an item from the trash | revert, roll back, undelete, recover | Taasta (taasta-, taaste-) |
| Compare; Compare as | Show the changes between versions | diff | Võrdle, Võrdlusviis (võrdl-, võrrel-) |
| Trash; Move to trash | Where deleted items wait | bin, recycle bin | Prügikast, Vii prügikasti (prügikast-) |
| Delete for good; Empty trash | The only actions that delete. Calm, exact, never playful | purge, delete forever | Kustuta jäädavalt (jäädav-); Tühjenda prügikast (tühjenda-) |
| upload | | transfer | laadi üles, üleslaadimine (laad-) |
| Shared with me | The page of items others shared with you | | Minuga jagatud (minuga jagatud-) |

### Sharing and access

| Term | Meaning | Never | Estonian |
|---|---|---|---|
| access; People with access | What a person or agent may do with an item | permission, grant, ACL | ligipääs, Kellel on ligipääs (ligipääs-) |
| Give access / Remove access / Change access | | grant, revoke | Anna ligipääs / Eemalda ligipääs / Muuda ligipääsu |
| Can view / Can comment / Can edit / Can manage | The roles on an item (`role.*`) | Viewer, Editor, Owner (on items) | Saab vaadata (vaada-, vaata-) / Saab kommenteerida (kommenteeri-) / Saab muuta (muut-, muud-) / Saab hallata (halla-, haldav-) |
| share | Give people access to an item | | jagama (jaga-) |
| link; Anyone with the link | A share link | URL token, secret URL | link, Kõik, kellel on link (link-, lingi-, linke-) |
| Turn off (a link, a key, an app password) | Stop it working | revoke, invalidate, disable | Lülita välja (lülit-) |
| Everyone in *Space* | All members of a space | | Kõik ruumis *Space* |
| from folder *Name* | Access that comes from a folder above | inherited | kaustast *Name* |

### Agents

| Term | Meaning | Never | Estonian |
|---|---|---|---|
| agent | An AI or program that works in Ruum for a person | bot, assistant, AI (alone), integration | agent (agent-, agend-) |
| *Agent* for *Person* | The by-line of every agent | | *Agent* kasutajale *Person* |
| app | A program that asks to connect (OAuth consent screen) | client | rakendus (rakendus-) |
| Connect / Allow / Don't allow | The consent actions | authorize | Ühenda (ühend-) / Luba (luba-, lubat-, lubam-) / Ära luba |
| key | An agent key for scripts and the CLI | token, API key (alone) | võti (võti-, võtme-, võtit-) |
| app password | A password for WebDAV clients (Finder, Explorer) | | rakenduse parool (parool-) |
| Pause / Resume | Stop and start an agent | disable, enable | Peata (peata-, peatu-) / Jätka (jätka-) |
| Undo changes | Undo an agent's run of changes | revert, rollback | Võta muudatused tagasi |
| run | Changes an agent made close together | batch, session | tööhoog (one go of work: "ühe hooga tehtud muudatused") |
| Where / Highest role / Ends on / Changes per hour | The parts of an agent's access | scope, max role, budget | Kus / Kõrgeim roll / Lõpeb / Muudatusi tunnis |
| signed-in device | A session on the Account page | session (as a noun on screen) | sisselogitud seade (seade-, seadme-) |
| sign-in method | Google, Microsoft or a passkey | identity, IdP | sisselogimisviis (sisselogimisvii-) |

### Databases (step 2, `ia.md`)

| Term | Meaning | Never | Estonian |
|---|---|---|---|
| database | A database item in Files | db, instance | andmebaas (andmebaas-) |
| query; Read only / Can change data | | statement, SQL console, write mode | päring (päring-); Ainult lugemiseks / Saab andmeid muuta |
| migration | A change to the tables | schema change (alone), DDL | migratsioon (migratsioon-) |
| branch | A copy of a database to try changes | fork, clone, sandbox | haru (haru-) |
| restore point | A moment you can go back to | snapshot, checkpoint | taastepunkt (taastepunkt-) |
| privacy class: Normal / Personal / Secret | What agents see of a column | data class, sensitivity | privaatsusklass: Tavaline / Isikuandmed / Salajane |
| stand-in | The value agents see instead of personal data | pseudonym, hash, token | asendusväärtus (asendusväärtus-) |
| hidden | A secret value agents never see | masked, redacted | peidetud |
| write access / personal data access | Short-time extra access for agents | write grant | kirjutusligipääs (kirjutusligipääs-) / isikuandmete ligipääs (isikuandmete ligipääs-) |
| request; Waiting for you; Approve / Deny | An agent asks a person | ticket | taotlus; Ootab sind; Kinnita / Keeldu |

## Do not translate

Keep these as they are, in every language, also inside sentences:

| Kind | Words |
|---|---|
| Product name | **Ruum** (the name, always with a capital letter; the Estonian word for a space is "ruum" in lower case: see `style-guides/et.md`, section 2) |
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

Each language has a style guide (formality, punctuation, numbers and dates, gender-neutral
wording) in `style-guides/<tag>.md`. Estonian: `style-guides/et.md`.
