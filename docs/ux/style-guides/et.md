# Estonian style guide (et)

Status: `provisional` (translator agent, 2026-11-08; design 12.4.1 and 18.9). How Ruum speaks
Estonian. Use it with the voice (`../copy.md`, section 1), the approved terms (`../glossary.md`,
column "Estonian"), the context of each message (`web/src/locales/context/en.json`) and the
localisation rules (`../i18n.md`). `web/scripts/check-translations.mjs` checks the parts that a
program can check.

## 1. Address and voice

- **Always "sina".** Write to one person, informally: "sa", "sina", "sinu", "sul". Buttons and
  instructions use the imperative singular: "Salvesta", "Vali kaust", "Laadi failid üles". Never
  "Teie" or "teie", never the plural imperative ("Valige", "Salvestage"), never the infinitive as
  a command ("Salvestada"). Decided by the person on 2026-11-08.
- **Playful where nothing is at stake** (tone `playful`: empty states, welcome, success toasts,
  small nudges). Short, warm, everyday words. A light idiom is welcome, but not one with "ruum"
  (section 2): "Siin on veel küllalt vaba kohta", not "vaba ruumi". A playful line still says the
  facts.
- **Calm and exact where something is at stake** (tone `calm`: errors, security, sign-in, access,
  money and plans, deleting for good, legal). No joke, no exclamation mark. Say what happened, then
  what to do: "Seda ei saa tagasi võtta. Ka selle versioonid kustutatakse."
- **Neutral** (labels, buttons, headings, lists): the plain word, nothing more.
- **One exclamation mark at most**, and only for a real success: "Oled sees! Tere tulemast
  ruumi {space}."
- **No officialese.** Not "antud" for "this" (use "see"), not "teostama", "sooritama",
  "vastav", "kõnealune", "käesolev", "tuleb + infinitive" as an order ("Teil tuleb sisestada").
  Not "Palun" before every instruction: an imperative is polite enough in Estonian.
- **No blame, no drama, no hype.** Not "Ups!", "Oih!", "Viga!", "Kehtetu", "Keelatud tegevus",
  "maagiline", "nutikas", "tehisintellektil põhinev".

| Tone | English | Estonian |
|---|---|---|
| playful | Plenty of room here. Drop files in, or upload them. | Siin on veel küllalt vaba kohta. Lohista failid siia või laadi need üles. |
| playful | You're in! Welcome to {space}. | Oled sees! Tere tulemast ruumi {space}. |
| playful | {name} is back at work. | {name} on jälle tööl. |
| calm | Copy the key now. Ruum shows it only once. | Kopeeri võti kohe. Seda näidatakse ainult üks kord. |
| calm | The migration failed. Nothing changed. | Migratsioon ebaõnnestus. Midagi ei muutunud. |
| calm | You can't undo this. Its versions are deleted too. | Seda ei saa tagasi võtta. Ka selle versioonid kustutatakse. |

## 2. "Ruum", the product, and "ruum", the space

"Ruum" is the product name, and "ruum" is also the approved Estonian word for a space
(`glossary.md`). Both may appear in Estonian text; the capital letter tells them apart. Decided by
the person on 2026-11-09 (decision 0005 B-1).

- **The product is "Ruum"**, with a capital letter, in every case form: Ruumi, Ruumis, Ruumiga.
  "Logi Ruumi sisse", "{app} soovib Ruumiga ühenduda", "Tere tulemast Ruumi, {name}".
- **A space is "ruum"**, all lower case, in every case form: ruumi, ruumis, ruumiga, and in
  compounds: meeskonnaruum, salvestusruum.
- **A sentence does not start with the common word "ruum".** At the start of a sentence a
  capital letter would make the space look like the product. Rephrase: put another word first.

  | Not this | This |
  |---|---|
  | Ruumil peab olema vähemalt üks omanik. | Igal ruumil peab olema vähemalt üks omanik. |
  | Ruumi {space} inimesed. | Inimesed ruumis {space}. |
  | Ruum {space} on jõudnud oma paketi liikmete piirini. | Liikmete arv on jõudnud ruumi {space} paketi piirini. |
  | Ruumi {space} muudatused ilmuvad siia. | Siia ilmuvad ruumi {space} muudatused. |

  This holds for labels, buttons and headings too, because they start with a capital letter:
  "Space settings" is „Selle ruumi seaded“, the space name field is „Selle ruumi nimi“, the space
  picker is „Vali ruum“, the column of spaces is „Asub ruumis“. Starting with "Sinu ruum ..." or
  "See ruum ..." works when it is true.
- **Avoid the two words side by side.** "Ruumi ruumid" makes the reader stop. Rephrase first
  ("Sind kutsutakse Ruumis ühte ruumi" keeps them apart). When no other wording works,
  "Ruum ruum" is fine.
- The product name may stand in running text, but an impersonal sentence is often shorter and
  reads better: English "Ruum shows it only once" becomes "Seda näidatakse ainult üks kord".
- Never use "ruum" in its everyday sense (room, free space) in the UI: write "vaba koht" or
  "salvestusruum" (storage).

`web/scripts/check-translations.mjs` (rule `ruum`) fails when a sentence or a branch of the
Estonian catalogue starts with the common word, and when a word "Ruum..." with a capital letter
stands where the English message does not name the product (that "Ruum" means a space). It warns
when the two words stand side by side.

## 3. Grammar around placeholders

Code never inflects a value. A placeholder (`{name}`, `{space}`, `{folder}`) is always in the
nominative, so the sentence must not need another case there.

- Put a noun that carries the case before the placeholder: "kausta {folder}" (into),
  "kaustas {folder}" (in), "ruumis {space}", "ruumiga {space}", "faili {name} versioonid",
  "agendile {name}", "aadressile {email}".
- Or put the placeholder after a colon: "Jagatud: {who}", "Kustuta jäädavalt: {name}". Use this
  in accessible names and lists, not in friendly sentences where a case noun reads better.
- A placeholder as the subject in the nominative needs nothing: "{name} on tagasi",
  "{name} pole veel midagi muutnud".
- When a value can be missing, the message has a `select` (`hasName`, `hasSpace`,
  `hasFolder`): translate every branch so each one is a whole sentence.
- Inline role words (`role.*.inline`, `memberRole.*.inline`, `audit.role.*`) are nouns or verb
  phrases that fit the sentence around them; in a sentence, quote a role that is a phrase:
  ligipääs „saab muuta“.

## 4. Plurals and numbers

- Estonian has two CLDR plural categories: `one` and `other`. Every plural has both.
- After a number other than 1, the noun is in the partitive singular and the verb is singular:
  "3 faili laaditi üles", "2 üksust on juba siin", "5 minutit tagasi". After 1: the nominative,
  "1 fail laaditi üles".
- Use digits for all numbers. Decimals take a comma and thousands a space ("1,5 MB",
  "12 345"): `Intl` does this, do not write numbers by hand. Keep the non-breaking space between a
  number and its unit.

## 5. Dates and times

- `Intl` formats dates and times. In text around them: "kell {time}" (at), "eile kell {time}",
  "kuni {date}" (until), "{date} kuni" never.
- 24-hour times ("14:00"). Weekdays and months are written in lower case ("esmaspäeval",
  "5. november 2026").
- Relative times: "äsja", "5 minutit tagasi", "2 tundi tagasi", "eile kell 14:00".

## 6. Punctuation and spelling

- **Quotation marks** „like this“ (U+201E and U+201C). Quote the name of a page, tab or button
  inside a sentence: jaotises „Minuga jagatud“. Do not quote names of items, people or spaces
  (the UI shows them in medium weight); emails quote them.
- **Commas** before a subordinate clause (et, kui, mis, kes, kus, kuni, sest, nagu, kuigi) and
  before "aga", "kuid", "vaid". No comma before "ja", "ning", "või", "ega" in a simple list.
- **Separators**: keep "·" between facts and " - " in window titles, as in English.
- **Ellipsis**: three dots "..." after a menu item that opens a dialog, as in English.
- **Compounds** are one word: meeskonnaruum, prügikast, sisselogimisviis, tegevuslogi. With an
  abbreviation, a name or a code, use a hyphen: "WebDAV-klient", "PDF-dokument", "{ext}-fail",
  "API-võti".
- **Case endings on foreign names** follow the pronunciation: Google'iga, Claude'i, Claude
  Code'is, Microsoftiga, ChatGPT-s.
- **Sentence case** in every title, button and label. No emoji.
- No abbreviations in the UI ("nt", "jne", "sh"): write the words out, or rewrite.

## 7. Gender-neutral wording

- "Ta" and "tema" are gender-neutral: use them for a person (English singular "they"). "Nad"
  only for several people.
- Use neutral nouns: kasutaja, inimene, liige, omanik, administraator, külaline. No compounds
  with "-mees" or "-naine".
- An agent or an app is "see", not "ta": "See ei saa midagi lugeda ega muuta".

## 8. Words and terms

- Use the approved terms of `glossary.md` (column "Estonian") on every screen. The check script
  fails when an English term has no approved Estonian word in its translation.
- Prefer Estonian words to loan words: "e-kiri" (not "meil"), "laadi üles", "laadi alla",
  "kopeeri", "kleebi", "lohista", "tehisintellekt" (not "AI" in running text).
- Keep what the do-not-translate list says (`glossary.md`): product names, protocols, codes,
  placeholders and tags. Other products' screens and buttons are written as those products show
  them; Claude and ChatGPT show English to most Estonian users, so keep their English names in
  quotes: „Settings“, „Connectors“, „Add custom connector“.
- Estonian runs about 10 to 30% longer than English. Stay within the length budget of each
  message (`maxLength` in the context file): pick the shorter word, never an abbreviation.

## 9. Status of a translation

Each message has a status in the catalogue metadata (`@metadata.status` of `et.json`):
`machine` (an agent translated it) or `agent-reviewed` (a reviewer agent on another model checked
it, design 12.4.1 step 4). Estonian is "beta" until every message is `agent-reviewed` and the key
journeys are checked on rendered screens.
