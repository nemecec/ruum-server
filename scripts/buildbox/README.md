# The build server

A Hetzner server (cax41: 16 ARM cores, 32 GB RAM, 320 GB disk, Ubuntu 24.04) where agents build
and test instead of the laptop. Each agent gets its own copy of its worktree, its own cargo
target, its own Postgres, its own data cluster and its own SpiceDB, so parallel agents do not
wait for or break one another. They share the rest of the dev stack (Garage, Zitadel, NATS and so
on of `scripts/dev.sh up`). The e2e scripts (`scripts/e2e_smoke.sh`, `scripts/e2e_browser.sh`)
also use the agent's own Postgres, data cluster and SpiceDB.

| File | What it does |
|---|---|
| `hcloud.py` | Creates, shows or deletes the server, its SSH key and its firewall (SSH only from one IP). |
| `cloud-init.yaml` | The first boot: Docker, build tools, Node.js 22, Rust for the user `ruum`. |
| `setup.sh` | Run once after a create, and again at any time: Node.js, base copy `~/ruum-server` from GitHub, dev stack, `sqlx-cli`, the PostgreSQL 17 client (`pg_dump` for the engine's export), Playwright's Chromium with its OS packages, status. |
| `run.sh` | Runs a command for one agent on the server. |

## Cost

The server costs money while it exists (cax41 in hel1: about EUR 0.08 per hour, so about EUR 58 for a full month).
Delete it when no run needs it:

```sh
python3 scripts/buildbox/hcloud.py delete
```

A new server needs `hcloud.py create --allow-ip <laptop IP>`, then the setup:

```sh
ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'bash -s' < scripts/buildbox/setup.sh
```

Set `BUILDBOX_HOST=<ip>` for `run.sh` when the IP is not the default of the script.

## Node.js and the browser

- Node.js comes from the official binaries (`nodejs.org/dist`), as one pinned version with its
  SHA-256, in `/opt/node`, with `node`, `npm`, `npx` and `corepack` linked into `/usr/local/bin`.
  The version is in `cloud-init.yaml` and in `setup.sh`; change both together. `setup.sh`
  installs it when it is missing or another version, so an older server gets it too.
- `setup.sh` runs `npm ci` in the base copy's `web/`, then `playwright install-deps chromium`
  (the OS packages, with sudo) and `playwright install chromium`. The browser goes to
  `~/.cache/ms-playwright`, which all agents share. The Playwright version is the one that
  `web/package-lock.json` pins. When it changes, an agent's `scripts/e2e_browser.sh` downloads the
  new browser itself; run `setup.sh` again when the OS packages change too.
- Each agent's `scripts/e2e_browser.sh` runs `npm ci` once in `~/w/<name>/web` (`run.sh` does
  not copy `node_modules/`, and keeps the one on the server).

## A fresh platform database

The shared stack's Postgres (volume `ruum_postgres`: the databases `ruum`, `zitadel` and
`spicedb`) can get a migration that `main` does not have, for example from an old branch. Then
everything that migrates the stack's `ruum` database fails at start ("migration N was
previously applied but has been modified"). `run.sh` does not use that database, but a command
that you run without its settings can. To start that database again:

```sh
ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'bash -s -- --fresh-platform-db' < scripts/buildbox/setup.sh
```

It refuses while an agent command runs. It stops the stack, keeps the old volume as a tar file
and the old Zitadel state and KEK in `~/backups/platform-db-<time>/`, and starts the stack with a
new volume, a new Zitadel instance and new OIDC apps. The agent Postgres containers and the data
cluster (`postgres-data`) and the agent data clusters (`pgdata-<name>`) do not change. Delete the backup when the new stack works.

## How agents use it

```sh
scripts/buildbox/run.sh <name> <worktree> -- <command...>
scripts/buildbox/run.sh <name> --stop
```

- `<name>`: one per agent, for example the branch name (`[a-z0-9_-]`).
- `run.sh` copies the worktree with rsync to `~/w/<name>`: tracked and untracked files with
  their uncommitted changes, not ignored files, `target*/`, `node_modules/` or `.git`. Files
  that are gone in the worktree are deleted on the server. There is no git repository on the
  server side.
- The command runs in `~/w/<name>` with the settings of `scripts/dev.sh env`, and:
  - `CARGO_TARGET_DIR=~/targets/<name>` and `CARGO_INCREMENTAL=0`;
  - its own Postgres (container `pg-<name>`, the stack's image, no fsync) in `DATABASE_URL` and
    `TEST_DATABASE_URL`. `#[sqlx::test]` names each test database after its test and drops
    older ones at start, so two agents in one Postgres break each other's tests;
  - its own SpiceDB (container `spicedb-<name>`, memory datastore) in `SPICEDB_ENDPOINT`,
    `RUUM_SPICEDB_ENDPOINT` and `RUUM_SPICEDB_HTTP_URL`, so branches with different schemas
    do not clash;
  - its own data cluster (container `pgdata-<name>`, see below) in `RUUM_DBHOST_URL`, with
    `DBHOST_CHECK_CONTAINER=pgdata-<name>` (for `scripts/dbhost_check.sh`) and
    `RUUM_DBENGINE_REQUIRE_POSTGRES=1`, so the data cluster tests fail instead of skipping when
    the cluster does not answer;
  - its own dev root `~/w/.<name>.dev` in `RUUM_DEV_ROOT` (for `scripts/e2e_browser.sh`), and
    its `scripts/dev.sh` in `RUUM_DEV_SH` (for `scripts/e2e_smoke.sh`). It is a wrapper of the
    base copy's `scripts/dev.sh`: its `env` prints the stack's settings, then the agent's
    Postgres, SpiceDB and data cluster. Next to it is a link to the stack's `deploy/dev/.env`;
  - free ports for the e2e scripts: `RUUM_E2E_PORT` and `RUUM_E2E_DAV_PORT`
    (`e2e_smoke.sh`), `RUUM_E2E_BROWSER_PORT`, `RUUM_E2E_BROWSER_DAV_PORT`, `RUUM_E2E_WEB_PORT`
    and `RUUM_E2E_MOCK_PORT` (`e2e_browser.sh`). A value that you give with `env` wins.
- The output streams back, and `run.sh` exits with the status of the command. The command gets no
  stdin. Ctrl-C or SIGTERM stops it on the server too.
- Give extra settings with `env`:

```sh
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- cargo clippy --workspace --all-targets -- -D warnings
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- cargo test --workspace
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- env RUUM_SUITE_BACKEND=postgres cargo test -p ruum-testkit
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- scripts/e2e_smoke.sh
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- scripts/e2e_browser.sh
```

- `scripts/e2e_browser.sh` keeps its work folder (the HTML report, traces of failed tests and
  logs) in `/tmp/ruum-e2e-browser.*` on the server. Copy it to the laptop with
  `rsync -a -e 'ssh -i ~/.config/ruum/buildbox_ed25519' ruum@<ip>:/tmp/ruum-e2e-browser.XXXXXX ./`.
- After a rebase, touch the sources before cargo runs (`run.sh` keeps the file times, so cargo
  can reuse a stale binary): `run.sh <name> <worktree> -- bash -c 'find crates contracts
  migrations -type f -exec touch {} + && scripts/e2e_browser.sh'`.

## The agent's data cluster

`run.sh` builds the image `ruum-buildbox-pgdata:<name>` from the tree's own
`deploy/dev/postgres-data` (its `Dockerfile`, entrypoint and `initdb/`), and starts the container
`pgdata-<name>` from it on a free port of 127.0.0.1, with the passwords of the stack's
`deploy/dev/.env`. So a branch that changes the data cluster set-up tests its own set-up, not
the one of `main`, and two agents never share databases or roles. There is no flag: every run
of a tree that has `deploy/dev/postgres-data` gets it. A tree without that folder (an old branch)
keeps the stack's shared data cluster, and the first line of the run says so.

- The image carries a hash of the folder (label `ruum.buildbox.pgdata-hash`). When the folder
  changes, the next run builds the image again and replaces the container with a new, empty
  cluster. Otherwise the run reuses the container and its databases.
- The superuser logs in only on the local socket, as in the stack:
  `docker exec pgdata-<name> psql -U postgres`.
- The settings are those of the tree's `Dockerfile` (with fsync), not the faster ones of
  `pg-<name>`: the cluster under test is the one the tree describes.

Why always, and not only on request (measured on the cax41, 2026-11-09):

| What | Cost |
|---|---|
| First run of a key (cached image layers: start and `initdb/`) | about 4 s |
| Later runs (container reused) | none measurable |
| Image build with no cache (the apt layer: PostGIS, pgvector) | 29 s, once per server and change of that layer |
| Disk: volume of a new cluster; image | about 60 MB; the image shares its layers with the other keys |
| Memory: idle cluster; limit | about 30 MB; 768 MB |

## Disk and cleanup

- A cargo target of the whole workspace takes several GB. Run `run.sh <name> --stop` when the
  agent's work is merged or dropped. It deletes `pg-<name>`, `pgdata-<name>` and
  `spicedb-<name>` with their data, the image `ruum-buildbox-pgdata:<name>`, `~/targets/<name>`, `~/w/<name>` and `~/w/.<name>.dev`. Each agent Postgres may use up
  to 2 GB of RAM.
- Keep at least 50 GB free. Check with
  `ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'df -h /; du -sh ~/targets/*'`, or run
  `setup.sh` again: it prints the disk, the agent containers and the targets.
- The dev stack is shared. Do not run `scripts/dev.sh clean` or `down` on the server while
  agents run. The Postgres suites make their own databases and drop them.
- Deleting the server deletes everything on it. Nothing there is the only copy: the code lives
  in the laptop worktrees and on GitHub.
