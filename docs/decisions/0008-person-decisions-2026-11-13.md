# 0008: decisions of the person, 2026-11-13

Decided by the person after the run 9 digest. Not provisional.

1. **Personal workspaces get random slugs (backlog S-35).** A personal tenant no longer takes its
   slug from the display name ("<name>'s space"); it gets a neutral random slug (for example
   `ws-7k2m9q`, lower-case, fits the slug rules of docs/STEP3.md "Host names and slugs"). The person
   may choose a readable slug explicitly; the UI says that the slug is public (DNS, certificate
   transparency logs) and permanent. Team tenants keep name-based slugs. The next-free-number
   sequence must not reveal other tenants' names (random suffixes instead of `-2`, `-3`). Before
   the first public app.
2. **The deploy controller (backlog O-34, option a).** A small deploy controller in the cluster,
   in its own namespace with its own ServiceAccount, takes a release request (the commit and the two
   image digests), verifies the provenance itself, runs the migration step and the rollout. The
   deployer may only write release requests; it loses the rights to create Pods and Jobs in `ruum`.
   A card in the next run. Until it is merged, the deployer kubeconfig stays on the person's Mac
   only: no CI job, agent or other person deploys.
3. **Cards may add CI lines for their own tests (backlog T-42, 2026-11-14).** A card may add lines
   to `.github/workflows/ci.yml` when its own tests need them (a database role, a service, an
   environment variable, a suite line, a "require" flag so a test fails instead of skipping). The
   reviewer checks those lines like code: they must only make the card's tests run (never skip,
   weaken or remove other jobs), and they name the card in a comment. Changes to CI beyond that
   (new jobs, triggers, permissions, secrets) stay a card of their own.
