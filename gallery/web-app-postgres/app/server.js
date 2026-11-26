// The app of the gallery template "Web app + Postgres + sign-in".
//
// - Sign-in: the Ruum gateway signs people in and sends a short-lived `Ruum-User` token with
//   every request. `withRuumUser` from `@ruum/app` checks it; the app has no password, no
//   session secret and no sign-in code. `user.sub` is a stable, pairwise ID of the person.
// - Database: the runtime gives the binding `db` as `RUUM_BINDING_DB` (a connection URL with a
//   short-lived role; no password in the archive). Every query is parameterised ($1, $2, ...).
// - Logs: one JSON object per line on stdout, a message template plus named fields. The classes
//   of the fields are in telemetry-privacy.yaml. Never log a token, a password or a note's text.

import http from "node:http";
import { withRuumUser } from "@ruum/app";
import pg from "pg";

const PORT = Number(process.env.PORT ?? 8080);
const MAX_BODY_BYTES = 64 * 1024;

/** A structured log line: the template is written as it is; values go into `fields`. */
function log(level, template, fields = {}) {
  const severity = { info: 9, warn: 13, error: 17 }[level] ?? 9;
  process.stdout.write(
    JSON.stringify({
      time: new Date().toISOString(),
      severity_number: severity,
      severity_text: level.toUpperCase(),
      template,
      fields,
    }) + "\n",
  );
}

if (!process.env.RUUM_BINDING_DB) {
  log("error", "RUUM_BINDING_DB is not set; bind the database as db in app.yaml");
  process.exit(1);
}
const pool = new pg.Pool({ connectionString: process.env.RUUM_BINDING_DB, max: 5 });

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function send(res, status, type, body) {
  res.writeHead(status, {
    "content-type": type,
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  });
  res.end(body);
}

async function readForm(req) {
  const chunks = [];
  let size = 0;
  for await (const c of req) {
    size += c.length;
    if (size > MAX_BODY_BYTES) throw Object.assign(new Error("too large"), { code: "body_too_large" });
    chunks.push(c);
  }
  return new URLSearchParams(Buffer.concat(chunks).toString("utf8"));
}

async function listNotes(user) {
  const { rows } = await pool.query(
    "select id, title, body, created_at from notes where author_sub = $1 order by created_at desc limit 100",
    [user.sub],
  );
  return rows;
}

function page(user, notes) {
  const items = notes
    .map((n) => `<li><strong>${escapeHtml(n.title)}</strong><p>${escapeHtml(n.body)}</p></li>`)
    .join("\n");
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Notes</title></head>
<body>
<h1>Notes</h1>
<p>Signed in as ${escapeHtml(user.name ?? "you")}.</p>
<form method="post" action="/notes">
  <label>Title <input name="title" required maxlength="200"></label>
  <label>Text <textarea name="body" maxlength="10000"></textarea></label>
  <button>Add</button>
</form>
<ul>${items}</ul>
</body></html>`;
}

// The routes that handle() answers; the request log names one of these or "other".
const ROUTES = ["/", "/notes"];

async function handle(req, res, user) {
  const url = new URL(req.url ?? "/", "http://app.invalid");
  if (req.method === "GET" && url.pathname === "/") {
    return send(res, 200, "text/html; charset=utf-8", page(user, await listNotes(user)));
  }
  if (req.method === "POST" && url.pathname === "/notes") {
    const form = await readForm(req);
    const title = (form.get("title") ?? "").trim().slice(0, 200);
    const body = (form.get("body") ?? "").slice(0, 10000);
    if (!title) return send(res, 422, "text/plain; charset=utf-8", "A note needs a title.\n");
    const { rows } = await pool.query(
      "insert into notes (author_sub, author_name, title, body) values ($1, $2, $3, $4) returning id",
      [user.sub, user.name ?? null, title, body],
    );
    log("info", "note {note_id} added", { note_id: rows[0].id });
    res.writeHead(303, { location: "/" });
    return res.end();
  }
  return send(res, 404, "text/plain; charset=utf-8", "Not found.\n");
}

const signedIn = withRuumUser(async (req, res, user) => {
  const started = Date.now();
  try {
    await handle(req, res, user);
  } catch (e) {
    log("error", "request failed with {error_code}", { error_code: e?.code ?? "unknown" });
    if (!res.headersSent) send(res, 500, "text/plain; charset=utf-8", "Something went wrong.\n");
  } finally {
    // The matched route, never the raw path: a path the client chose can hold an email or a token.
    const pathname = new URL(req.url ?? "/", "http://app.invalid").pathname;
    const path = ROUTES.includes(pathname) ? pathname : "other";
    log("info", "{method} {path} answered {status} in {duration_ms} ms", {
      method: req.method,
      path,
      status: res.statusCode,
      duration_ms: Date.now() - started,
    });
  }
});

http
  .createServer((req, res) => {
    // The platform's health check comes without a user; it only says that the app runs.
    if (req.method === "GET" && req.url === "/healthz") return send(res, 200, "text/plain", "ok\n");
    return signedIn(req, res);
  })
  .listen(PORT, () => log("info", "listening on {port}", { port: PORT }));
