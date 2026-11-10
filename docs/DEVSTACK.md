# The laptop stack

This is the local development stack of `docs/DESIGN.md` section 12.2. It runs in Docker Compose
(project name `ruum`). The files are in `deploy/dev/`. You control it with `scripts/dev.sh`.

You need Docker with Compose, `openssl`, `curl` and `jq`.

## Commands

| Command | What it does |
|---|---|
| `scripts/dev.sh up` | On the first run, copies `deploy/dev/.env.example` to `deploy/dev/.env` with new random secrets. Later runs add the settings that `.env.example` has and `.env` lacks (for example the passwords of `postgres-data`). Makes the local KEK file `deploy/dev/.state/tenant-keys.kek` if it is missing (see "Tenant keys"). Starts the stack, waits until all services are healthy, and runs the init steps (Garage buckets `ruum-dev` and `ruum-sqlite` and the key, Zitadel project and OIDC apps, the OpenBao transit engine and key). You can run it again at any time. |
| `scripts/dev.sh up --step4` | The same, and also starts Forgejo (compose profile `step4`). |
| `scripts/dev.sh up --sso-mock` | The same, and also starts the mock OIDC provider (compose profile `sso-mock`) and adds it to Zitadel as "Mock SSO". The flags can go together. See "Sign-in providers". |
| `scripts/dev.sh idps` | Configures the sign-in providers of Zitadel again (`scripts/zitadel_idps.sh`). `up` does this too. |
| `scripts/dev.sh idps check` | Starts the flow of each provider on the Zitadel login page without a password and stops at the provider. See "Sign-in providers". |
| `scripts/dev.sh down` | Stops the stack. The data stays. |
| `scripts/dev.sh clean` | Stops the stack and deletes all volumes and `deploy/dev/.state/`, with the local KEK file, and the local SQLite files in `deploy/dev/data/sqlite/`. It keeps `.env`. |
| `scripts/dev.sh logs [service]` | Follows the logs. |
| `scripts/dev.sh status` | Shows the services and their health. |
| `scripts/dev.sh psql [db]` | Opens `psql` as the `ruum` role. The default database is `ruum`. |
| `scripts/dev.sh seed` | Creates the missing roles, databases and extensions, and runs the set-up of the data cluster again. The seed of tenants, users, an agent key and sample files comes with the core service. |
| `scripts/dev.sh env` | Prints the settings for the Ruum server. Use `eval "$(scripts/dev.sh env)"`. `RUUM_DBHOST_URL` is the data cluster as `ruum_dbhost`. `RUUM_LOCAL_KEK_FILE` is the local KEK file. `RUUM_SQLITE_DIR` is the directory of the SQLite files and `RUUM_SQLITE_BUCKET` their replica bucket (see "SQLite storage"). |
| `scripts/dev.sh ca` | Copies the root certificate of Caddy's local CA to `deploy/dev/.state/caddy-root.crt`. |

## Ports

All ports bind to `127.0.0.1`. The stack does not use 5433, 8080, 8787 or 8788 (they belong to
`kadakas-server`).

| Port | Service |
|---|---|
| 80, 443 | Caddy: `https://ruum.localhost`, `https://*.ruumapps.localhost` (80 redirects to 443) |
| 5434 | Postgres 17 with PostGIS and pgvector (the platform database) |
| 5435 | `postgres-data`: the data cluster for customer databases (`POSTGRES_DATA_PORT` in `.env` changes it) |
| 3900 | Garage S3 API (region `garage`, path-style URLs) |
| 3903 | Garage admin API (token `GARAGE_ADMIN_TOKEN`) |
| 8180 | Zitadel (issuer `http://localhost:8180`, console `http://localhost:8180/ui/console`) |
| 50051 | SpiceDB gRPC |
| 8543 | SpiceDB HTTP gateway |
| 4222 | NATS client (JetStream on) |
| 8222 | NATS monitoring |
| 8200 | OpenBao (dev mode) |
| 8025 | Mailpit web UI and API |
| 1025 | Mailpit SMTP |
| 3001, 2222 | Forgejo web and SSH (profile `step4` only) |
| 8190 | The mock OIDC provider, issuer `http://oidc-mock.localhost:8190/ruum` (profile `sso-mock` only) |
| 8490 | Reserved for the Ruum server on the host (`RUUM_BIND`, the server default). Caddy proxies `https://ruum.localhost` to it. |

## Services

- **Postgres** (`deploy/dev/postgres/`): the official `postgres:17` image with the PGDG packages
  of PostGIS and pgvector. The init script makes the roles `ruum` (with `CREATEDB`), `zitadel` and
  `spicedb`, and the databases `ruum`, `ruum_test`, `zitadel` and `spicedb`. The `ruum` role is
  not a superuser. Thus the script creates the extensions `postgis` and `vector` in `ruum`,
  `ruum_test` and `template1`. The databases that `#[sqlx::test]` creates come from `template1`,
  so they have the extensions too. Use `TEST_DATABASE_URL` or `DATABASE_URL` from `dev.sh env`.
- **postgres-data** (`deploy/dev/postgres-data/`): the data cluster (`docs/STEP2.md`
  "Infrastructure work" 1 and 2). Customer databases live here, never in the platform database.
  The image `ruum-dev/postgres-data:17` is Postgres 17.11 with PostGIS and pgvector, without
  PostgreSQL Anonymizer (decision 0004-4).
  - `template1` holds no extension. `postgis` and `vector` are marked trusted in their control
    files, so `ruum_dbhost` creates them (in the schema `extensions` of a database) without being
    a superuser (decision 0004-34 g).
  - `max_prepared_transactions = 0` (decision 0004-34 d). `pg_stat_statements` is not loaded.
    WAL archiving is on (`archive_mode = on`); until card I-2 adds pgBackRest, the archive
    command keeps nothing.
  - The init script (`initdb/10-ruum-data.sh`, run again by `dev.sh up` and `seed`) makes the
    role `ruum_dbhost` with `CREATEDB` and `CREATEROLE`, no superuser, password
    `RUUM_DBHOST_PASSWORD` in `.env`. It revokes `CONNECT` and `TEMPORARY` on `postgres` and
    `template1` from `PUBLIC`, and every right on the schema `public` of `template1`.
  - `pg_hba.conf` is written at each container start. `ruum_dbhost` and the per-database roles
    (`r_` and 16 hex) log in only with SCRAM and only from the platform's address: the gateway of
    the container's own network `data`, where connections to the host port come from. Set
    `RUUM_DATA_PLATFORM_CIDR` in `.env` to change it. The superuser (`POSTGRES_DATA_PASSWORD`)
    logs in only on the local socket: `docker compose -p ruum exec postgres-data psql -U postgres`.
    No other login works.
  - The engine (card E-2), not the stack, creates the databases and their roles.
  - `scripts/dbhost_check.sh` checks all of this on the running cluster (see "Check the stack").
- **Garage**: one node with a 2 GB layout. `dev.sh up` imports the access key from `.env` and
  creates the buckets `ruum-dev` (files) and `ruum-sqlite` (SQLite replicas), and allows the key
  on both.
- **litestream** (`deploy/dev/litestream/litestream.yml`): replicates the SQLite files of
  `RUUM_SQLITE_DIR` to the bucket `ruum-sqlite`. No port. See "SQLite storage".
- **Zitadel**: uses its own database in the same Postgres. The first start creates the instance
  `Ruum`, the organization `Ruum` and two users:
  - the human admin `admin@ruum.localhost`, password `ZITADEL_ADMIN_PASSWORD` in `.env`;
  - the service account `ruum-bootstrap` (role IAM owner). Its token is in
    `deploy/dev/.state/zitadel/bootstrap.pat`.

  `dev.sh up` then creates the project `Ruum` with two apps and writes their IDs to
  `deploy/dev/.state/zitadel/apps.env`:
  - `ruum-web`: a public OIDC client (authorization code with PKCE, refresh token, device code).
    The redirect URIs are `https://ruum.localhost/v1/auth/callback` and
    `http://127.0.0.1:8490/v1/auth/callback` (the server's callback of the contract),
    `https://ruum.localhost/auth/callback`, `http://localhost:5173/auth/callback` and
    `http://127.0.0.1:8976/callback` (for the CLI). Dev mode is on. A stack made before the
    `/v1/auth/callback` URIs were added needs `scripts/dev.sh clean` and `up` to get them.
  - `ruum-api`: an API client with a client secret, for token introspection by the Ruum server.

  Zitadel uses the login UI that is built into it (login v2 is off), so the stack needs no
  separate login container. Zitadel sends mail to Mailpit.
- **SpiceDB**: Postgres datastore in the database `spicedb`. The one-shot service
  `spicedb-migrate` runs `migrate head` before SpiceDB starts. The preshared key is
  `SPICEDB_PRESHARED_KEY` in `.env`.
- **NATS**: JetStream with at most 64 MB memory store and 2 GB file store.
- **OpenBao**: dev mode. It is unsealed, keeps its data in memory and loses it on a restart.
  The root token is `BAO_DEV_ROOT_TOKEN` in `.env`. `dev.sh up` enables the transit engine
  (`transit/`) and the key `ruum-tenant-keys` (type `aes256-gcm96`) at each start, because a
  restart loses them. The laptop server does not use them (see "Tenant keys"); the OpenBao test
  of `crates/ruum-core/src/privacy/keys.rs` does (`BAO_ADDR` and `BAO_TOKEN`, or
  `RUUM_OPENBAO_ADDR` and `RUUM_OPENBAO_TOKEN`; it is skipped without them).
- **Mailpit**: catches all mail. Open `http://localhost:8025`.
- **Caddy**: the HTTPS front. It uses its own local CA for `ruum.localhost`,
  `dav.ruum.localhost` and `*.ruumapps.localhost`. `https://ruum.localhost/caddy-health`
  answers `ok` from Caddy. The paths `/v1`, `/healthz`, `/.well-known` and `/mcp` go to the
  Ruum server on the host port 8490 (`RUUM_UPSTREAM` in `.env`). The WebDAV host
  `https://dav.ruum.localhost/` goes to the server's WebDAV listener on the host port 8492
  (`RUUM_DAV_UPSTREAM` in `.env`, `RUUM_DAV_BIND` of the server; decision 0002-3). It has no
  API and no web UI, and the platform origin has no `/dav`. An existing `.env` without
  `RUUM_DAV_UPSTREAM` uses the default port; recreate the Caddy container to load the new
  Caddyfile (`docker compose ... up -d caddy`). All other paths go to the web UI, Vite on the host port
  5173 (`RUUM_WEB_UPSTREAM` in `.env`; see `web/README.md`). The content domain
  `*.content.ruumapps.localhost` (download and preview links) also goes to the Ruum server, with
  the Host header kept. If an upstream does not run, Caddy answers 502 with a short message. The
  other `*.ruumapps.localhost` hosts answer 503 until the app gateway exists (step 3).
- **Forgejo**: only with `--step4`. It uses SQLite for now; step 4 decides the database.
- **oidc-mock**: only with `--sso-mock`. `mock-oauth2-server` of NAV, for automated tests of
  sign-in through an external provider. See "Sign-in providers".

Memory limits: Postgres 1 GB, postgres-data and Zitadel 768 MB each, SpiceDB, Garage, NATS, OpenBao and litestream 256 MB each,
Mailpit and Caddy 128 MB each, Forgejo 512 MB, oidc-mock 256 MB. When idle, the stack uses about 350 MB.

## SQLite storage

SQLite databases are files on the db host's disk (`docs/STEP2.md` "Infrastructure work" 4,
card I-3). On the laptop the directory is `deploy/dev/data/sqlite/` (ignored by git).
`dev.sh up` makes it, `dev.sh env` prints it as `RUUM_SQLITE_DIR`, and `dev.sh clean` deletes it.

### Directory layout

```text
$RUUM_SQLITE_DIR/
  <tenant>/              one directory per tenant, random name (for example 16 hex)
    <database>.db        one file per database, random name, WAL mode
    <database>.db-wal    made by SQLite
    <database>.db-shm    made by SQLite
```

Litestream replicates every file that matches `*.db` at any depth. So a database file must end
in `.db`, and nothing else in the directory may end in `.db`.

### The service

The compose service `litestream` runs Litestream v0.5.17
(`litestream/litestream:0.5.17`, pinned by digest in `docker-compose.yml`). One process
watches the whole directory (`dir`, `recursive`, `watch` in `litestream.yml`):

- A tenant directory or a database file that appears after the start is found within about
  1 s and replicated (sync every 1 s). The test below measured 1 to 2 s.
- A file that goes away is dropped from replication (`removed database from replication` in the
  log), and the process keeps running. The replica stays in the bucket.
- **Delete in two steps.** Litestream reads the `-wal` file when it drops a database. If the
  `-wal` file is already gone, the drop fails (`ERROR ... unregister database from store ...
  cannot read wal header`), Litestream keeps the path as known, and a new file at that path (for
  example a restore) is not replicated until Litestream restarts (seen with v0.5.17,
  `cmd/litestream/directory_watcher.go` `removeDatabase`). So delete the `.db` file first, wait
  until `litestream list` no longer shows it, then delete `-wal` and `-shm`.
- The replica of `<tenant>/<database>.db` is `s3://ruum-sqlite/<tenant>/<database>.db/` (LTX
  files). Litestream keeps its own state in the volume `litestream-meta`
  (`meta-dir`), not in `RUUM_SQLITE_DIR`.
- The healthcheck runs `litestream list`, which asks the running process on its control socket
  (`/var/run/litestream.sock`, inside the container only). Nothing listens on TCP.
- It uses the stack's S3 key (`S3_ACCESS_KEY_ID`) and reaches Garage as `http://garage:3900`.

To restore by hand (the latest state, or `-timestamp 2026-11-10T12:00:00Z` for a time):

```sh
eval "$(scripts/dev.sh env)"
docker run --rm --network ruum_default -v "$PWD/out:/out" \
  -e AWS_ACCESS_KEY_ID="$S3_ACCESS_KEY_ID" -e AWS_SECRET_ACCESS_KEY="$S3_SECRET_ACCESS_KEY" \
  litestream/litestream:0.5.17 restore -o /out/restored.db \
  's3://ruum-sqlite/<tenant>/<database>.db?endpoint=http://garage:3900&region=garage&forcePathStyle=true'
```

`litestream restore` refuses an output file that exists (use `-force`). Restore to a new file
outside `RUUM_SQLITE_DIR` when you only want to look; restore to the original path to bring a
lost database back (Litestream then picks the file up again and replicates new writes, when the
old file was deleted in two steps, see above).

### macOS: the writer and Litestream must share one kernel

SQLite in WAL mode coordinates processes with file locks and the shared memory file `-shm`.
Those do not cross the boundary between macOS and the Linux VM of Docker Desktop. Seen on
2026-11-10: with `sqlite3` on the macOS host writing a file in the bind mount, the Litestream
container's read transaction was invisible to it; the closing `sqlite3` checkpointed and
removed the WAL behind Litestream (`sync error ... invalid wal header magic: 0`, and later
writes were not replicated). Host-side deletes were also not seen (Docker Desktop passes no
delete events). On Linux (the build server, CI, production) the host and the container share
the kernel, and all of this works.

So on a Mac, the process that writes the SQLite files must run in a container too, or Litestream
must run on the host. `scripts/sqlite_restore_test.sh` does every SQLite step in a container,
so it is valid on both. `dev.sh up` prints a note on macOS. This is open for card E-3 (see the
notes below).

### The test

`scripts/sqlite_restore_test.sh` checks the replication end to end. It creates a tenant directory
and a database file with random names in WAL mode after Litestream started, writes rows, and
waits until a restore of the replica has them (at most 10 s). It notes the time, deletes rows,
restores to the noted time into a new file and finds the rows. Then it deletes the file locally (in
two steps), checks that Litestream still runs, restores the latest state to the original path,
finds the file, its rows and `integrity_check = ok`, and checks that a new write to the restored
file reaches the replica.

It never changes the stack's services: it starts its own container `litestream-<key>` (key:
`BUILDBOX_NAME` on the build server, else `SQLITE_TEST_KEY`, else `local`) with the same image and
`litestream.yml`, on the network of the stack's Garage, with its own temporary directory and its
own prefix `ruum-sqlite/<key>-<random>/`. It creates the bucket only when it is missing. At exit,
also on a failure, it removes the container, the directory and the objects under its prefix.
It needs `docker`, `curl` (with `--aws-sigv4`), `python3` and `openssl`.

```sh
scripts/sqlite_restore_test.sh                                     # laptop stack
scripts/buildbox/run.sh i-3 <worktree> -- scripts/sqlite_restore_test.sh   # build server
```

### Notes for the engine (card E-3)

What holds for Litestream v0.5.17 (source of `db.go`, release notes v0.5.3 to v0.5.17):

- **WAL mode is required.** Set `PRAGMA journal_mode = WAL` when the engine creates a file.
- **Litestream holds a long read transaction** on each database, so no checkpoint can pass the
  WAL position it has not copied yet. It releases it only for its own checkpoints.
- **Litestream does the checkpoints.** Its own connection has `wal_autocheckpoint = 0`. It runs
  `PASSIVE` checkpoints at 1000 WAL pages and every minute, and a blocking `TRUNCATE` checkpoint
  only as a backstop at 121359 pages (about 500 MB; `truncate-page-n`).
- **The engine must not run `TRUNCATE` or `RESTART` checkpoints**, and must not delete or
  truncate the `-wal` file. A blocking checkpoint waits on Litestream's read transaction (busy),
  and a WAL that changes behind Litestream's back breaks its position (as seen on macOS, it then
  stops replicating). Leave SQLite's automatic `PASSIVE`
  checkpoint on, or set `wal_autocheckpoint = 0` and let Litestream do it (the upstream advice for
  a high write rate).
- **Litestream must see every connection** (same kernel, see "macOS" above).
- **Deleting a database:** close every connection, delete the `.db` file, wait until Litestream
  has dropped it (`litestream list` on its socket, or about 2 s), then delete `-wal` and `-shm`
  (see "Delete in two steps" above; with the wrong order the path is stuck until a restart of
  Litestream). The replica stays in the bucket: the engine (or a retention job) must delete the prefix
  `<tenant>/<database>.db/` when the database's restore window ends.
- **Restore** reads the replica (`litestream restore`, by URL as above, with `-timestamp` for a
  point in time). Restore into a new file, then move it into place while no connection is open.
- Names: the files must match `*.db` (the `pattern` of `litestream.yml`).

## Tenant keys

Each tenant has a pseudonym key in the platform table `tenant_keys`, stored only wrapped by a
key-encryption key (KEK; decision 0004-16, `crates/ruum-core/src/privacy/keys.rs`). Production
wraps with OpenBao transit (`ruum-tenant-keys`). Dev OpenBao loses its keys on a restart, so the
laptop uses a local KEK file instead:

- `deploy/dev/.state/tenant-keys.kek`: 32 random bytes, mode 0600. `dev.sh up` makes it when it
  is missing and never overwrites it. `dev.sh env` prints its path as `RUUM_LOCAL_KEK_FILE`.
- `dev.sh down` and `up` keep it, so a restart of the stack keeps the tenant keys and the
  pseudonyms. `dev.sh clean` deletes it together with the platform database volume.
- If the file is lost while the volume stays, the stored tenant keys cannot be unwrapped. `dev.sh
  up` warns about it. Run `dev.sh clean` to start over.
- `wrapper_from_env` of `keys.rs` (card D-1 wires it into the server) picks OpenBao transit
  only when `RUUM_OPENBAO_ADDR` is set, else the file of `RUUM_LOCAL_KEK_FILE`. `dev.sh env` does
  not set `RUUM_OPENBAO_ADDR`.

## Trust the local CA

Without this step, browsers show a certificate warning for `https://ruum.localhost`.

1. Run `scripts/dev.sh ca`. It writes `deploy/dev/.state/caddy-root.crt`.
2. On macOS, add it to the login keychain:

   ```sh
   security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db deploy/dev/.state/caddy-root.crt
   ```

   On Linux, copy it to `/usr/local/share/ca-certificates/ruum-dev.crt` and run
   `sudo update-ca-certificates`.
3. For tools that do not use the system store, give the file directly, for example
   `curl --cacert deploy/dev/.state/caddy-root.crt https://ruum.localhost/caddy-health`,
   or `SSL_CERT_FILE` / `NODE_EXTRA_CA_CERTS`.

`scripts/dev.sh clean` deletes the CA. After a clean, trust the new root certificate again.

## Sign-in providers

Ruum does not talk to Google or Microsoft. Zitadel does: they are upstream identity providers
of the platform issuer (design 4.2). `scripts/zitadel_idps.sh` sets them up through the Zitadel
API, as the bootstrap service account. `dev.sh up` runs it after the Zitadel init step, and
`dev.sh idps` runs it alone. You can run it again at any time.

| Provider | Added when | Zitadel type |
|---|---|---|
| Google | `GOOGLE_OAUTH_CLIENT_ID` and `GOOGLE_OAUTH_CLIENT_SECRET` are set | Google, scopes `openid email profile` |
| Microsoft | `MS_OAUTH_CLIENT_ID` and `MS_OAUTH_CLIENT_SECRET` are set | Microsoft (Azure AD), tenant `common`: work, school and personal accounts; scopes `openid email profile User.Read` |
| Mock SSO | the mock OIDC provider answers (`dev.sh up --sso-mock`) | generic OIDC, issuer `http://oidc-mock.localhost:8190/ruum` |

How it works:

- The keys come from the environment, else from `deploy/dev/.env`, else from
  `~/.config/ruum/secrets.env` (`RUUM_SECRETS_FILE` changes the path). The script reads only
  these four names from that file. It never sources the file, and it never prints, logs or
  writes the values. They go to Zitadel on the standard input of `curl`. You do not have to copy
  them into `deploy/dev/.env`; if you do, that file wins. The keys are made in
  `docs/setup/ACCOUNTS.md` steps 4 and 5.
- Each provider is an instance provider, on the default login policy of the instance. The
  Zitadel login page then shows a button for it. The script also makes sure that the login
  policy allows external providers.
- It is idempotent. It finds a provider by its name and updates it, so a new secret takes effect
  at the next run. Without the keys it changes nothing: the stack starts as before, and a
  provider that was added earlier stays. Remove one in the console
  (`http://localhost:8180/ui/console`, Default settings > Identity providers).
- Provider options (design 4.2): the first sign-in makes a Zitadel account. For Google and
  Microsoft, the person first confirms the name and email on a Zitadel page, where they can also
  link the provider to an existing account by signing in to it. Zitadel never links accounts by
  email on its own, because that allows account takeover. Zitadel updates the name and email
  from the provider at each sign-in.
- Microsoft does not prove that a work account owns the email address in its profile. So the
  Microsoft provider has `emailVerified: false`, and Zitadel asks the person to verify the
  address once (the code goes to Mailpit on the laptop). Google sends `email_verified`.
- The redirect URIs that the Google and Microsoft apps must accept are
  `http://localhost:8180/ui/login/login/externalidp/callback` (the login UI of the stack) and
  `http://localhost:8180/idps/callback` (login v2). `docs/setup/ACCOUNTS.md` lists them with the
  staging and production ones.
- The sign-in buttons of the Ruum web UI all open the Zitadel login page, because the server
  does not send the `idp` hint to Zitadel yet. Choose the provider there.

`scripts/dev.sh idps check` checks the set-up without a password. For each provider on the login
policy, it starts a login of the `ruum-web` client like a browser, follows the redirects of
Zitadel with `curl`, and stops at the provider. It prints the status codes, the provider's host
and path, whether `client_id` is the configured one, and `redirect_uri`. Then it makes one
request to the provider: Google must answer `302` to its sign-in page (an unknown client goes to
`/signin/oauth/error`), and Microsoft must show its sign-in page for the app (an unknown client
gives error code 50059 or 700016). It never prints a secret.

### The mock OIDC provider

`dev.sh up --sso-mock` starts `mock-oauth2-server` (`ghcr.io/navikt/mock-oauth2-server`) on
port 8190, and the script adds it to Zitadel as "Mock SSO". The browser test
`web/e2e/sso.e2e.ts` signs in through it from start to end (see `docs/TESTING.md`). The CI job
`e2e-browser` starts the stack with `--sso-mock`.

- Its sign-in page asks for any user name (the subject) and optional claims as JSON, for
  example `{"email": "sam@example.test", "email_verified": true, "given_name": "Sam",
  "family_name": "Test"}`. Zitadel takes the claims from the ID token and makes the account
  without the confirm page.
- The issuer is `http://oidc-mock.localhost:8190/ruum`. The mock takes the issuer from the Host
  header. Browsers and `curl` send `*.localhost` to the loopback address, so they reach the host
  port. Zitadel reaches the container through the same name, an alias in the compose network
  `sso`. So both sides see the same issuer.
- Zitadel's HTTP client refuses private addresses (its SSRF deny list: `10.0.0.0/8`,
  `172.16.0.0/12`, `192.168.0.0/16`, loopback and others). The network `sso` therefore uses
  `203.0.113.0/24` (TEST-NET-3 of RFC 5737, never routed on the internet). Zitadel is on the
  networks `default` and `sso`. The deny list stays as it is. A stack made before this network
  existed gets it at the next `dev.sh up`, which recreates the Zitadel container once.
- The mock accepts any client ID and secret. The values in the script are test values.
- When the mock is stopped, the "Mock SSO" button stays on the login page and fails. Run
  `dev.sh up --sso-mock` again, or remove the provider in the console.

### What a person must still check by hand

The automated checks stop at the provider's sign-in page. Only a person with real accounts can
finish a Google or Microsoft sign-in. Do this once on the laptop stack, and again when a key
changes (the Microsoft secret expires after 24 months):

1. `scripts/dev.sh idps check` passes.
2. Start the web UI and the server (`web/README.md`), open the sign-in page, choose "Sign in with
   Google", then "Google" on the Zitadel page, and sign in with your own Google account. Check:
   Zitadel shows the confirm page with your name and email; after it, you land on your files in a
   new personal space; Account shows your Google address as verified.
3. The same with "Microsoft", once with a personal Microsoft account and once with a work or
   school account, if you have one. Check: Zitadel asks you to verify the email (the code is in
   Mailpit, `http://localhost:8025`), then you land on your files.
4. The other points of the manual checklist in `docs/TESTING.md` (an existing user with the same
   verified email, email grants).

## Manual steps

- Trust the Caddy root certificate (see above).
- Passkeys are not set up in Zitadel. A person adds one in the console (`docs/TESTING.md`,
  manual checklist).
- A real Google and Microsoft sign-in (see "What a person must still check by hand").

## Check the stack

`scripts/dbhost_check.sh` checks the data cluster as the platform uses it. As `ruum_dbhost` it
creates two databases with the roles of every kind, and from sessions that log in as each role it
shows that no role reaches another role or another database, that logins come only from the
platform's address, and that `ruum_dbhost` creates every extension of the manifest allow-list. It
prints `PASS` or `FAIL` per check, drops what it made and exits non-zero on a failure. It needs
`psql` and `jq`.

```sh
eval "$(scripts/dev.sh env)"
echo 'select 1; select extname from pg_extension' | scripts/dev.sh psql
scripts/dbhost_check.sh
scripts/sqlite_restore_test.sh
curl --aws-sigv4 "aws:amz:garage:s3" --user "$S3_ACCESS_KEY_ID:$S3_SECRET_ACCESS_KEY" -T CLAUDE.md "$S3_ENDPOINT/$S3_BUCKET/check.txt"
curl --aws-sigv4 "aws:amz:garage:s3" --user "$S3_ACCESS_KEY_ID:$S3_SECRET_ACCESS_KEY" "$S3_ENDPOINT/$S3_BUCKET/check.txt"
docker compose -p ruum exec spicedb grpc_health_probe -addr=localhost:50051
curl -X POST http://127.0.0.1:8543/v1/schema/read -H "Authorization: Bearer $SPICEDB_PRESHARED_KEY" -d '{}'
curl http://localhost:8180/debug/healthz
curl http://localhost:8180/.well-known/openid-configuration
curl 'http://127.0.0.1:8222/healthz?js-enabled-only=true'
curl http://127.0.0.1:8200/v1/sys/health
curl http://127.0.0.1:8025/api/v1/info
curl --cacert deploy/dev/.state/caddy-root.crt https://ruum.localhost/caddy-health
```
