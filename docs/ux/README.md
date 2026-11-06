# UX design of Ruum

Status: `provisional` (design section 18.9), 2026-11-06. Written by the UX designer agent for
step 1 (Core). A person can veto or change any part. Synthetic findings stay provisional until
real users confirm them.

The UX designer works one step ahead of the UI implementers (design section 18.4.1). These pages
are the input for UI task cards and the reference for the UX review panel.

| Page | What it holds |
|---|---|
| [personas.md](personas.md) | The four people we design for in step 1. Also the briefs for the synthetic users. |
| [journeys.md](journeys.md) | The six step 1 journeys: steps, screens, success criteria and every state. |
| [ia.md](ia.md) | Navigation, URLs and the names of things (one word per concept). |
| [design-system.md](design-system.md) | Tokens (colour, type, spacing, radius, motion) and components with states and accessibility rules. |
| [copy.md](copy.md) | UI copy rules and the exact copy of main messages, errors and empty states. |
| [agent-experience.md](agent-experience.md) | Rules for MCP tool names and descriptions, CLI output and error hints. |
| [gap-analysis.md](gap-analysis.md) | Where the UI on `main` differs from this design, as prioritised task cards. |
| [decisions.md](decisions.md) | What the UX fix round (`q/uxfix`) did per card, its decisions, and the new contract needs C-7 to C-11. |
| [databases/](databases/README.md) | Step 2 (databases): journeys J7 to J14, screens, decisions DB-1 to DB-11 and contract needs C-15 to C-23. The components, copy, names and agent rules are in the pages above (sections marked step 2). |
| [tools/](tools/) | Throwaway scripts: screenshots of the UI with the mock API, and the colour contrast check. |

## How to use these pages

- A UI task card links the journey, names the components from the design system, lists the
  states, and copies the exact strings from `copy.md`.
- The UX review panel checks a UI branch against these pages: screenshots at desktop (1280 px) and
  phone (390 px) width, light and dark, keyboard only, and axe.
- A change to a name in `ia.md` changes `copy.md` and the task cards in the same commit.

## Tools

```sh
cd web && npx vite --mode mock --port 5183 --strictPort   # the UI with the mock API
cd docs/ux/tools && npm install && node screenshots.mjs   # 57 screenshots to docs/ux/tools/shots/
node docs/ux/tools/contrast.mjs                           # WCAG contrast of the colour tokens
```

The screenshot script uses an installed Playwright Chromium and never downloads a browser.
