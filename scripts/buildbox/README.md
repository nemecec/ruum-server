# The build server

A Hetzner server (cax41: 16 ARM cores, 32 GB RAM, 320 GB disk, Ubuntu 24.04) where agents build
and test instead of the laptop. Each agent gets its own copy of its worktree, its own cargo
target, its own Postgres and its own SpiceDB, so parallel agents do not wait for or break one
another. They share the rest of the dev stack (Garage, Zitadel, NATS and so on of
`scripts/dev.sh up`). `scripts/e2e_smoke.sh` uses the stack's own Postgres and SpiceDB.

| File | What it does |
|---|---|
| `hcloud.py` | Creates, shows or deletes the server, its SSH key and its firewall (SSH only from one IP). |
| `cloud-init.yaml` | The first boot: Docker, build tools, Rust for the user `ruum`. |
| `setup.sh` | Run once after a create, and again at any time: base copy `~/ruum-server` from GitHub, dev stack, `sqlx-cli`, status. |
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
  - free ports for `scripts/e2e_smoke.sh`.
- The output streams back, and `run.sh` exits with the status of the command. The command gets no
  stdin. Ctrl-C or SIGTERM stops it on the server too.
- Give extra settings with `env`:

```sh
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- cargo clippy --workspace --all-targets -- -D warnings
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- cargo test --workspace
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- env RUUM_SUITE_BACKEND=postgres cargo test -p ruum-testkit
scripts/buildbox/run.sh feat-x ../ruum-server-wt/feat-x -- scripts/e2e_smoke.sh
```

## Disk and cleanup

- A cargo target of the whole workspace takes several GB. Run `run.sh <name> --stop` when the
  agent's work is merged or dropped. It deletes `pg-<name>` and `spicedb-<name>` with their
  data, `~/targets/<name>` and `~/w/<name>`. Each agent Postgres may use up to 2 GB of RAM.
- Keep at least 50 GB free. Check with
  `ssh -i ~/.config/ruum/buildbox_ed25519 ruum@<ip> 'df -h /; du -sh ~/targets/*'`, or run
  `setup.sh` again: it prints the disk, the agent containers and the targets.
- The dev stack is shared. Do not run `scripts/dev.sh clean` or `down` on the server while
  agents run. The Postgres suites make their own databases and drop them.
- Deleting the server deletes everything on it. Nothing there is the only copy: the code lives
  in the laptop worktrees and on GitHub.
