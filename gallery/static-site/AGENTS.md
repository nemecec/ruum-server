# Working on this project

This project came from a Ruum gallery template. These rules are for every agent and person who
changes it. Ruum checks some of them at deploy time; the rest keep the people who use the app
safe.

## Logs: structured, never personal

- Write one JSON object per line on stdout: a message template and named fields, for example
  `{"template": "order {order_id} paid", "fields": {"order_id": 42}}`. Use `@ruum/log` or the
  small `log()` function in `server.js`.
- Never put a value into the template string. Ruum classifies each field by its name.
- Never log a token, a password, a cookie, a session, a whole request body or the text that a
  person typed.
- Every field name that the code writes has a line in `telemetry-privacy.yaml` with its class
  (`normal`, `personal` or `secret`). A field without a line counts as `personal`. A person
  confirms new fields when the app is published.

## SQL: parameters only

- Every query uses parameters (`$1`, `$2`, ...). Never build SQL from strings with values in
  them, also not for "safe" values such as numbers or IDs.
- Change the schema only with a new migration in `migrations/` (`NNNN_name.sql`, the next
  number). Never edit a migration that ran.
- Every migration that adds a column starts with a classes header for each new column:

  ```sql
  -- ruum:classes public.notes.created_at=normal public.notes.title=personal
  ```

  The classes are `normal`, `personal` (names, emails, free text) and `secret` (tokens,
  hashes). Agents see `personal` values only as pseudonyms.

## Secrets

- Never put a secret in the archive: not in the code, not in `app.yaml`, not in a `.env` file,
  not in a test.
- Sign-in comes from Ruum: the gateway checks who the person is and sends a signed `Ruum-User`
  token with every request (`@ruum/app` checks it). The app needs no password, no session key
  and no sign-in code.
- The database comes through the binding `db` (`RUUM_BINDING_DB`, a connection URL with a
  short-lived role). There is never a database password in the archive.
- If the app needs a key of another service, declare it under `secrets` in `app.yaml`; a person
  with `manage` enters the value in Ruum.

## Previews and publishing

- Every deploy makes a **preview** first: its own address, a masked branch of the database
  (agents and testers see pseudonyms, not real personal data), and no entered secrets.
- **Publishing** makes a ready preview the live app. An agent's publish waits for a person's
  approval: say what changed and why, and wait.
- Look at the preview and at its logs before you ask for a publish.

## Reference

- Every field of `app.yaml`: the `app.yaml` reference of the gallery
  (`gallery/reference/app-yaml.md` in the Ruum repository). The first line of `app.yaml` names
  the schema, so an editor checks the file as you type.
- The SDK (`@ruum/app`) is vendored in `vendor/ruum-app`.
