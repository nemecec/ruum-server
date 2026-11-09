# Ruum product direction

Status: draft: awaiting the person's approval. Drafted by the architect panel's proposer on
2026-11-10 from `docs/DESIGN.md`, the decisions in section 17 and in `docs/decisions/` (0001 to 0005),
and the UX package (`docs/ux/`). Design 20.4: this document is owned by the person. The person
approves it once and reviews it in the monthly review (design 19.4). Agents do not change it; they
may propose a change as a pull request with a reason.

**What it is for.** The product manager agent scores every idea against this page (design 20.2), so
the product loop does not just follow the most requests. The support agent cites it when it declines
a request (design 22.3). A brief that goes against a principle here says so in its first line.

## 1. Vision

**Your files, photos, databases and apps in one tree, with one permission system, built so that you
and your AI agents can work in it safely.**

Today a small team keeps files in one service, photos in another, a database somewhere else and the
app that an agent built for them in a fourth place, each with its own sharing rules. Ruum puts all of
them in one tree. Sharing a folder shares everything in it. Every change is a version, so every change,
also an agent's, can be undone.

AI agents are a main user, not an add-on. An agent can store files, make a database, deploy an app
on it and keep its source, through the same API, MCP server and CLI that people use. It acts with its
own identity, for a named person, within a scope that the person chose. People can always see what
an agent did and take it back.

Ruum is affordable because its prices come from its costs, and its data stays in the EU.

## 2. Target users

Decision 17-2: **individuals and small teams who build with agents.** The personas
(`docs/ux/personas.md`) are hypotheses until real users confirm them.

| Who | What they need from Ruum | Persona |
|---|---|---|
| A professional who works alone and is not technical | find the newest file fast, share a project with a client, trust undo, let a helper agent tidy things and see what it did | Liis |
| The admin of a small team (2 to 25 people) | who has access and why, remove a leaver in one step, a predictable bill | Marko |
| A developer who works with coding agents | connect an agent in minutes with a narrow scope, a CLI and errors an agent can act on, a database and an app without running servers | Jonas |
| A guest who only receives shares | open a link without an account, know what they may do | Helena |
| The agents of all of these | one model to learn (nodes), declarative manifests, good error codes and hints, narrow tokens, budgets with a hard stop | (the agent benchmark) |

**Not the first users:** large enterprises with procurement, compliance teams and 24/7 support needs;
people who want anonymous free hosting of public sites. Ruum can grow towards larger teams (SSO, SCIM,
dedicated cells), but no step is planned around them before the first users are served well.

## 3. What Ruum is not

| Ruum is not | So an idea like this is declined or parked |
|---|---|
| A general-purpose cloud (no virtual machines, no arbitrary containers with open ports, no Kubernetes for customers) | "Let me run my own Redis cluster", "give me SSH" |
| A free host for anonymous public sites | public apps without an approved, known tenant (decision 17-4, 17-22, design 13) |
| A code host that competes with GitHub on features | stars, social feeds, issue boards for open-source communities; Ruum hosts the repos of its users' projects (step 4) |
| An AI model provider | Ruum does not train models and does not sell model access at a margin; the AI gateway passes cost through (design 21, decision 17-37) |
| An advertising or data business | no ads, no selling or mining customer data, no training on customer content |
| A service that takes what the market allows | prices follow costs plus a published markup (section 4) |
| An agent that acts on its own | every agent acts for a named person, within a scope and a budget that the person set (design 3.4, decision 17-13) |
| A 24/7 enterprise promise, at first | best-effort support until there is a team (decision 17-5); the agent operations of design 19 aim higher, but the promise follows the proof |
| A replacement for desktop tools | Kadakas CAD and other programs keep their documents in Ruum through providers (design 7); Ruum does not rebuild them in the browser |
| A place where one tenant's data meets another's | no cross-tenant joins, shared data never moves to the guest's tenant (design 4.1, 14) |

## 4. Principles

Each principle names where it was decided. When two principles conflict, the earlier one in this list
wins, and the brief says so.

### 4.1 Safety and trust

1. **Nothing is lost by accident.** Every change is a version; deleting goes to the trash first; the
   UI says how to get things back, and says "for good" only when it is for good (design 5, 11,
   `docs/ux/copy.md`).
2. **Safe by default, fail closed.** A column that nobody classified counts as `personal`; an action
   that the operations policy does not list is red; a guard that does not answer escalates to a
   person (design 8.5, 19.3, `docs/ops/policy.yaml`).
3. **Agents see what they need, not more.** Read-only by default; personal values as pseudonyms;
   secrets as the hidden-value marker; a personal column of a tenant-defined type masked like
   `secret`, never as a pseudonym (design 8.5, decision 0005 B-2). Raw values only through an explicit,
   time-limited, logged grant.
4. **Text from agents and users is data, never instructions.** Every field that one agent writes and
   others read is marked untrusted (decision 0005 B-3); alerts, logs and tickets are untrusted for the
   operations agents too (design 19.3).
5. **Agents are visible, never disguised.** Every action records `actor` and `on_behalf_of`; the UI
   shows "agent name for person name" with the agent mark (design 3.4, decision 0003-5).
6. **One permission system for everything.** A new kind of resource is a typed node behind the
   provider contract, with the same roles and sharing (design 1, 7). A feature that needs its own
   permission model is a warning sign.
7. **Tenant isolation is tested, not assumed.** The isolation and permission suites pass for every
   endpoint, on every change (design 15, CLAUDE.md).
8. **People decide what cannot be undone, costs money, or faces outward.** Deleting tenant data,
   changing security policy, spending above budget, telling customers about a breach, publishing,
   pricing (design 18.9, 19.3, 20.3).

### 4.2 Privacy and place

9. **Data in the EU.** Ruum starts in Helsinki; backups stay in the EU (decision 17-5, design 13).
10. **Privacy-aware by default.** App telemetry keeps errors and Web Vitals only, end users as pseudonyms,
    no IP addresses stored (design 9.6). Full modes are a clear choice of the app owner.
11. **Portable.** Every tenant can export everything; moving a tenant between cells uses the same export
    (design 14, 15). Open protocols where they exist: S3, WebDAV, MCP, OAuth, OCM later.

### 4.3 Money

12. **Cost-based pricing.** Price = marginal unit cost × (1 + markup). The markup covers fixed costs and
    a small reserve, and it is published: "our price is our cost plus X %". Prices change quarterly,
    with smoothing (design 21.4).
13. **Every cost is recorded and explainable.** The cost ledger can answer "why does this cost X?";
    each space sees its own breakdown (design 21.1, 21.3, 21.5).
14. **Tracking costs less than what it tracks**: below 5 % of each resource class's cost (design 21.2).
15. **Budgets with hard stops**, especially for agents. An agent in a loop must not run up a bill
    (design 11, 21.5).
16. **The free tier is a deliberate cost**, funded by the markup, with hard limits and no public apps
    (decision 17-4, design 21.4).
17. **Every brief names its cost**: running cost, and the cost of building it in agent tokens and time
    (design 21.6).

The markup, the free tier and the rounding rules are decisions of the person (design 21.4). Until the
cost agent has real costs from staging, the plans of design 4.3 are placeholders.

### 4.4 Voice and look

18. **One voice in every language: informal and playful, never official or corporate.** Informal address
    everywhere (Estonian "sina", German "du"). Playful where nothing is at stake; calm and exact, with no
    jokes, where data, security, money or legal matters are at stake (decided by the person on 2026-11-08;
    `docs/ux/copy.md` section 1, design 12.4).
19. **Plain words.** No tenant, node, grant, scope, purge or token in the UI; short sentences, active
    voice; no hype ("magic", "smart", "AI-powered", "seamless"); no blame, no drama; names over IDs
    (`docs/ux/copy.md`, `docs/ux/ia.md`).
20. **"Ruum" is the product, "ruum" is a space.** The capital letter tells them apart in Estonian; a
    sentence does not start with the common word (decision 0005 B-1).
21. **The look is Graphite**: the precise instrument, cool graphite and fjord blue, square corners, IBM
    Plex, applied through the design tokens only (decision 0005 B-5). Colour never carries meaning
    alone; the agent colour belongs only to agents; no emoji; no sparkle for agents.
22. **Accessible and small-screen first.** WCAG 2.2 AA; every journey works at 390 px and on a slow
    connection (`docs/ux/personas.md`).

### 4.5 How Ruum is built and run

23. **Agents first, consumer features after.** The order of work builds what agents need (files,
    databases, apps, repos) before photos and sync (design 16).
24. **Contracts first; additive changes only.** The contract is the source of truth; a breaking
    change needs a new version (design 18.1).
25. **A small team.** Ruum runs on a self-healing platform, an agent swarm for the exceptions, and a
    person for judgment (design 19). A feature that needs a human operator on call is a cost to name
    in its brief.
26. **Open-source building blocks, no lock-in to one provider.** Postgres, SpiceDB, Zitadel, NATS,
    OpenBao, the Grafana stack; the S3 API for blobs; a second backup provider (design 12.1, 13).

## 5. How the product manager scores an idea

For each idea, the brief answers these questions. A "no" on any of the first three declines or parks
the idea unless the person decides otherwise.

| # | Question | Source |
|---|---|---|
| 1 | Does it serve one of the target users of section 2 (or their agents)? | section 2 |
| 2 | Does it stay inside what Ruum is (section 3)? | section 3 |
| 3 | Does it keep every principle of section 4.1 and 4.2? | section 4 |
| 4 | Does it fit the one tree and the one permission system, or does it add a second model? | principle 6 |
| 5 | What does it cost to run per unit, and to build in agent tokens and time? Can the price follow the cost? | principles 12 to 17 |
| 6 | How many users and agents asked for it, with what evidence (tickets, retros, benchmark failures, usage)? | design 20.1, 20.2 |
| 7 | Can an agent use it through the API, MCP and CLI, not only the UI? | design 11 |
| 8 | Does it add work for the operators, or a new red action for the person? | principle 25, design 19 |

The score is a recommendation. The person accepts, rejects or defers each brief on the weekly product
review page (design 20.2).

## 6. Open questions for the person

- **The markup and the free tier.** Not set yet; a decision brief follows when staging has real costs.
- **The second user group.** Section 2 puts small teams and developers on the same level. If one should
  come first for the next steps, say which; the order of work (design 16) does not change.
- **The "not an AI model provider" line.** The AI gateway (decision 17-37) is still open. This draft
  assumes pass-through pricing at cost plus the same published markup.
- **Public projects** (design 9.9) are planned after restricted folders. This draft keeps "not a free
  host for anonymous public sites" and allows public projects of known, approved tenants.
