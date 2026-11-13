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
