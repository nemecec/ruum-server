# The laptop stack

This is the local development stack of `docs/DESIGN.md` section 12.2. It runs in Docker Compose
(project name `ruum`). The files are in `deploy/dev/`. You control it with `scripts/dev.sh`.

You need Docker with Compose, `openssl`, `curl` and `jq`.

## Commands

| Command | What it does |
|---|---|
| `scripts/dev.sh up` | On the first run, copies `deploy/dev/.env.example` to `deploy/dev/.env` with new random secrets. Starts the stack, waits until all services are healthy, and runs the init steps (Garage bucket and key, Zitadel project and OIDC apps). You can run it again at any time. |
| `scripts/dev.sh up --step4` | The same, and also starts Forgejo (compose profile `step4`). |
| `scripts/dev.sh down` | Stops the stack. The data stays. |
| `scripts/dev.sh clean` | Stops the stack and deletes all volumes and `deploy/dev/.state/`. It keeps `.env`. |
| `scripts/dev.sh logs [service]` | Follows the logs. |
| `scripts/dev.sh status` | Shows the services and their health. |
| `scripts/dev.sh psql [db]` | Opens `psql` as the `ruum` role. The default database is `ruum`. |
| `scripts/dev.sh seed` | Creates the missing roles, databases and extensions. The seed of tenants, users, an agent key and sample files comes with the core service. |
| `scripts/dev.sh env` | Prints the settings for the Ruum server. Use `eval "$(scripts/dev.sh env)"`. |
| `scripts/dev.sh ca` | Copies the root certificate of Caddy's local CA to `deploy/dev/.state/caddy-root.crt`. |

## Ports

All ports bind to `127.0.0.1`. The stack does not use 5433, 8080, 8787 or 8788 (they belong to
`kadakas-server`).

| Port | Service |
|---|---|
| 80, 443 | Caddy: `https://ruum.localhost`, `https://*.ruumapps.localhost` (80 redirects to 443) |
| 5434 | Postgres 17 with PostGIS and pgvector |
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
| 8090 | Reserved for the Ruum server on the host. Caddy proxies `https://ruum.localhost` to it. |

## Services

- **Postgres** (`deploy/dev/postgres/`): the official `postgres:17` image with the PGDG packages
  of PostGIS and pgvector. The init script makes the roles `ruum` (with `CREATEDB`), `zitadel` and
  `spicedb`, and the databases `ruum`, `ruum_test`, `zitadel` and `spicedb`. The `ruum` role is
  not a superuser. Thus the script creates the extensions `postgis` and `vector` in `ruum`,
  `ruum_test` and `template1`. The databases that `#[sqlx::test]` creates come from `template1`,
  so they have the extensions too. Use `TEST_DATABASE_URL` or `DATABASE_URL` from `dev.sh env`.
- **Garage**: one node with a 2 GB layout. `dev.sh up` imports the access key from `.env` and
  creates the bucket `ruum-dev`.
- **Zitadel**: uses its own database in the same Postgres. The first start creates the instance
  `Ruum`, the organization `Ruum` and two users:
  - the human admin `admin@ruum.localhost`, password `ZITADEL_ADMIN_PASSWORD` in `.env`;
  - the service account `ruum-bootstrap` (role IAM owner). Its token is in
    `deploy/dev/.state/zitadel/bootstrap.pat`.

  `dev.sh up` then creates the project `Ruum` with two apps and writes their IDs to
  `deploy/dev/.state/zitadel/apps.env`:
  - `ruum-web`: a public OIDC client (authorization code with PKCE, refresh token, device code).
    The redirect URIs are `https://ruum.localhost/auth/callback`, `http://localhost:5173/auth/callback`
    and `http://127.0.0.1:8976/callback` (for the CLI). Dev mode is on.
  - `ruum-api`: an API client with a client secret, for token introspection by the Ruum server.

  Zitadel uses the login UI that is built into it (login v2 is off), so the stack needs no
  separate login container. Zitadel sends mail to Mailpit.
- **SpiceDB**: Postgres datastore in the database `spicedb`. The one-shot service
  `spicedb-migrate` runs `migrate head` before SpiceDB starts. The preshared key is
  `SPICEDB_PRESHARED_KEY` in `.env`.
- **NATS**: JetStream with at most 64 MB memory store and 2 GB file store.
- **OpenBao**: dev mode. It is unsealed, keeps its data in memory and loses it on a restart.
  The root token is `BAO_DEV_ROOT_TOKEN` in `.env`.
- **Mailpit**: catches all mail. Open `http://localhost:8025`.
- **Caddy**: the HTTPS front. It uses its own local CA for `ruum.localhost` and
  `*.ruumapps.localhost`. `https://ruum.localhost/caddy-health` answers `ok` from Caddy. All other
  paths go to the Ruum server on the host port 8090 (`RUUM_UPSTREAM` in `.env`). If the server
  does not run, Caddy answers 502 with a short message. `*.ruumapps.localhost` answers 503 until
  the app gateway exists (step 3).
- **Forgejo**: only with `--step4`. It uses SQLite for now; step 4 decides the database.

Memory limits: Postgres 1 GB, Zitadel 768 MB, SpiceDB, Garage, NATS and OpenBao 256 MB each,
Mailpit and Caddy 128 MB each, Forgejo 512 MB. When idle, the stack uses about 350 MB.

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

## Manual steps

- Trust the Caddy root certificate (see above).
- Passkeys, Google and Microsoft sign-in are not set up in Zitadel yet. Do this in the console
  as `admin@ruum.localhost` when the sign-in work starts, or add it to the init step.

## Check the stack

```sh
eval "$(scripts/dev.sh env)"
echo 'select 1; select extname from pg_extension' | scripts/dev.sh psql
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
