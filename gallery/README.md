# The Ruum gallery

The templates that ship with Ruum (`docs/DESIGN.md` 2.7, decision 0006 point 11). The server
loads them once at start from the directory in `RUUM_GALLERY_DIR` (default `gallery` relative to
the working directory; `/app/gallery` in the server image). They are never nodes of a tenant:
every signed-in person and agent reads them with `listTemplates` and `getTemplate`
(`gallery:<slug>`).

| Slug | Kind | What it makes |
|---|---|---|
| `web-app-postgres` | project | An `Issues` folder, a Postgres database with a migration and sample data, and a Node app with sign-in through Ruum |
| `static-site` | project | A static site |
| `internal-tool` | project | A `Files` folder and a Node app that shows its files (read only) |
| `issue-bug`, `issue-feature`, `issue-question` | issue | The fields of a bug, a feature request, a question |
| `issue-error` | issue | The fields of the issues that monitoring opens for a new error |

`reference/app-yaml.md` is the `app.yaml` reference, generated from
`contracts/manifests/app.schema.json`.

## Add a template (self-hosting)

1. Make a directory whose name is the slug: lower-case letters, digits and `-`, at most 63
   characters (`[a-z0-9][a-z0-9-]{0,62}`).
2. Put a `template.yaml` in it (`contracts/manifests/template.schema.json`). A project
   template's `setup` names its parts:
   - `folder`: a folder of the new project (`path`), optionally copied from a folder of the
     template (`from`);
   - `database`: a database manifest file, a folder of migrations and a sample data file. The
     sample data holds invented rows only, never real personal data;
   - `app`: an archive `<name>.tar.gz`. Put the app's files in the folder `<name>/` and the
     server packs it at start (the same bytes every time), or put the archive file there.
3. Put an `AGENTS.md` next to it (copy one of these).
4. Restart the server.

The server checks every template at start: the schema, that every path in `setup` exists inside
the template folder, that the app archive unpacks within the smallest plan's limits and that its
`app.yaml` passes the manifest rules, that the database manifest is valid and that each
migration's `-- ruum:classes` header parses. Symlinks are refused, never followed. A template
that fails stops the start with an error that names the template and the field. To skip a
failing template with a warning instead, set `RUUM_GALLERY_STRICT=false`.

Never put a secret in a template: the gallery is readable by everybody who can sign in.
