# Personas

Status: `provisional`. These personas come from decision 2 ("individuals and small teams who
build with agents"), design section 18.9 and the step 1 scope. No real users were interviewed.
Treat every goal and frustration as a hypothesis until a usability test with real people
confirms it.

Each persona is also the brief of a **synthetic user** (design 18.9): a persona agent that drives
the real UI in a browser, thinks aloud and reports task success, steps and errors. The section
"Brief for the synthetic user" is the text to give that agent.

The names are fictional.

## Summary

| | Liis | Marko | Jonas | Helena |
|---|---|---|---|---|
| Role | Freelance landscape architect | Admin of a 6-person studio | Developer with Claude Code | Helper who receives shares |
| Technical skill | Low | Medium | High | Low |
| Main surface | Web UI, phone | Web UI | CLI, MCP, a little web UI | A link in an email, phone |
| Space | Personal | Team (owner) | Personal + guest in a team | None of her own at first (guest) |
| Agents | One, set up for her | Two team helpers, owned by people | Several, made and thrown away | None |
| Biggest fear | Losing a client drawing | A leaver who keeps access | An agent that breaks things unseen | Doing something wrong in someone else's files |

## 1. Liis, freelance landscape architect (not technical)

**Who.** 41, works alone from a home studio in Tartu. Designs gardens and small public squares.
Has 30 to 60 active client projects a year. Uses a CAD program, photos from site visits, PDFs
and spreadsheets for plant lists. Her nephew set up Claude for her; she asks it to sort photos,
rename files and write plant lists. She does not know what a "token" or an "API" is.

**Context.** Works on a laptop in the studio and on a phone at the site. Mobile data at the site
is often slow or gone. She shares a folder per project with each client, and with one or two
contractors.

**Goals.**

- Find the newest drawing of a project in under 10 seconds, also on the phone.
- Share one project folder with a client by email, without the client making an account first.
- Get back yesterday's drawing when the newest one is wrong.
- Let her AI helper tidy files, and see in plain words what it did.
- Undo what the helper did when it got it wrong.

**Frustrations.**

- "Version 3 final FINAL.pdf". She keeps copies because she does not trust the history.
- Tools that use words she does not know: tenant, node, scope, grant, purge, token.
- Uploads that stop on site without telling her, so she thinks a file is safe when it is not.
- Dialogs that ask her to decide things she does not understand ("Inherit from parent?").
- Shared links that she cannot take back.

**What success looks like.** She stops keeping copies. She trusts that every change can be
undone, including the helper's changes.

**Brief for the synthetic user.** "You are Liis, a freelance landscape architect. You are not
technical. You do not know words like tenant, node or token, and you say so when you see them.
You use a laptop and a phone with a slow connection. Read only what is on the screen. Say aloud
what you expect before you click. Stop and say 'I am stuck' when you do not know what to do."

## 2. Marko, small team admin

**Who.** 36, co-owner and office manager of a 6-person architecture and planning studio. Not a
developer, but the "IT person" of the studio. Pays the bills. Sets up accounts, removes people
who leave, and answers "where is the file?".

**Context.** The studio has a team space with project folders, two interns each summer, and
outside consultants as guests. Two people use AI agents; one agent writes the weekly project
report.

**Goals.**

- Invite a new person and give them the right folders in one sitting (under 5 minutes).
- Remove a leaver and be sure that they, their agents and their links lose access at once.
- See who shared what with people outside the studio.
- Know how much storage is used and what the plan allows.
- Keep "everyone can share" on for the team, but turn off public edit links.

**Frustrations.**

- He cannot tell who has access to a folder, or why (direct or inherited).
- Settings with no explanation of their effect.
- An audit log that is a wall of IDs.
- Agents that look like people in the history, so he cannot tell who did a change.
- Paying for a plan without knowing what is used.

**What success looks like.** He can answer "who can see the Kalamaja project?" and "what did the
report agent change this week?" in one screen each.

**Brief for the synthetic user.** "You are Marko, the admin of a small studio. You are careful
and a little worried about access. You check every setting before you save. You want to know the
effect of each change. Say aloud when a screen does not tell you who will be affected."

## 3. Jonas, developer using Claude Code

**Who.** 29, freelance full-stack developer. Builds small web tools for clients. Uses Claude Code
every day in the terminal, and wants his agents to keep project files, notes and later databases
and apps (steps 2 and 3) in one place.

**Context.** Works in the terminal and the editor. Opens the web UI to set up agents, check a
diff, or undo an agent's run. Has a personal space, and is a guest in two client team spaces.

**Goals.**

- Connect Claude Code to Ruum over MCP in under 3 minutes, with a narrow scope (one folder).
- Use the `ruum` CLI in scripts, with readable output and `--json` for tools.
- See every change an agent made, and undo one run without losing his own work.
- Get errors that say what to do next, in words an agent can follow.
- Give each agent its own key that ends on a date.

**Frustrations.**

- OAuth flows that end on a blank page or a code to copy by hand.
- Error messages without a code, or with a hint for another surface ("GET /v1/me" when he is in
  MCP).
- Tool lists with 50 tools and vague descriptions, so the agent picks the wrong one.
- CLI output that prints IDs instead of names.
- A UI that hides advanced facts (IDs, paths, versions) when he needs them.

**What success looks like.** He tells Claude "put the meeting notes in Ruum under
Clients/Pärn" and it works on the first try. When it goes wrong, he undoes the run in two clicks.

**Brief for the synthetic user.** "You are Jonas, a developer who uses Claude Code. You are fast
and impatient. You read error messages closely and judge if an agent could act on them. You try
the keyboard first. Say aloud when the UI hides a fact that you need, such as a path or an ID."

## 4. Helena, helper who only receives shares

**Who.** 63, Liis's client, a retired teacher who is building a garden. Or a contractor who gets
a folder of plans. She has no Ruum account and does not want one. She opens what she is sent.

**Context.** Gets an email "Liis shared Garden at Pärna 12 with you" or a link in a chat. Opens it
on a phone or a tablet. Sometimes she must upload a photo of the site or leave a comment.

**Goals.**

- Open the plans from the email in one tap and read them.
- Download a PDF to print it.
- Upload two photos into the folder when Liis asks.
- Know what she is allowed to do, so she does not break anything.

**Frustrations.**

- Being asked to sign up before she can see anything.
- Screens full of buttons she may not use.
- Not knowing whether she can change things, or whether Liis sees her changes.
- Links that ask for a password she never got.

**What success looks like.** She opens the plans without help and never sees a screen meant for
the owner.

**Brief for the synthetic user.** "You are Helena. Someone sent you a link. You have no account
and you do not want one. You use a phone. You are afraid to break someone else's files, so you
avoid any button that you do not understand. Say aloud what you think each button does before you
press it."

## What the personas mean for the design

1. **Plain words first** (Liis, Helena). The UI never shows tenant, node, grant, scope, purge or
   token. `ia.md` maps the design words to UI words.
2. **Trust through undo** (Liis, Marko, Jonas). Every destructive action says how to get things
   back. Agent changes are always marked and can be undone.
3. **Who has access, and why** (Marko). The share dialog shows direct and inherited access, with
   the folder it comes from.
4. **Agents are visible, never disguised** (all). An agent always shows as "agent name for
   person name", with the agent mark.
5. **Two depths** (Jonas). The default view is simple; the details panel shows path, ID and
   version facts for those who need them, without a separate "advanced mode".
6. **Phone and slow networks** (Liis, Helena). Every journey works at 390 px width and tells the
   truth about uploads when the connection drops.
7. **Least surprise for guests** (Helena). A viewer never sees an action that they cannot do.
