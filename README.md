# Ruum

Ruum (`ruum.space`) is a multi-tenant cloud service. It stores files and photos, and hosts databases
and web apps. All of them are nodes in one tree, protected by one permission system. AI agents are a
main user.

Read `docs/DESIGN.md` first. The decisions are in section 17. The way we build (contracts, tests, task
cards, agents) is in section 18.

## Layout

- `crates/`: the Rust workspace (`cargo build --workspace`, `cargo test --workspace`).
- `contracts/`: the API, authz and event contracts. See `contracts/README.md`.
- `migrations/`: SQL migrations (sqlx).
- `web/`: the web UI (`npm install && npm run dev` in `web/`).
- `docs/STEP1.md`: the work areas of step 1 and the folders each one owns.

Run the core service without Docker: `cargo run -p ruum-server` (listens on `127.0.0.1:8490`; set
`RUUM_BIND` to change it, and `DATABASE_URL` to use Postgres and run the migrations).

## License

Ruum is licensed under the Apache License 2.0. See `LICENSE`.
