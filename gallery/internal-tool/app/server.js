// The app of the gallery template "Internal tool": a read-only view of the files of one folder.
//
// - Sign-in: the Ruum gateway signs people in and sends `Ruum-User`; `withRuumUser` checks it.
// - Files: the binding `files` (role view) gives the folder's node ID as RUUM_BINDING_FILES. The
//   app lists it through the runtime API (`http://ruum.internal`) with its workload token
//   (`runtimeFetch` adds it). There is no write path: the binding is view only.
// - Logs: one JSON object per line, a template plus named fields (telemetry-privacy.yaml).
//   File names are untrusted text: they are escaped in the page and never logged.

import http from "node:http";
import { runtimeFetch, withRuumUser } from "@ruum/app";

const PORT = Number(process.env.PORT ?? 8080);
const FOLDER = process.env.RUUM_BINDING_FILES;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function log(level, template, fields = {}) {
  const severity = { info: 9, warn: 13, error: 17 }[level] ?? 9;
  process.stdout.write(
    JSON.stringify({ time: new Date().toISOString(), severity_number: severity, severity_text: level.toUpperCase(), template, fields }) + "\n",
  );
}

if (!FOLDER || !UUID.test(FOLDER)) {
  log("error", "RUUM_BINDING_FILES is not a node ID; bind the folder as files in app.yaml");
  process.exit(1);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function send(res, status, type, body) {
  res.writeHead(status, { "content-type": type, "cache-control": "no-store", "x-content-type-options": "nosniff" });
  res.end(body);
}

async function children(folder) {
  const r = await runtimeFetch(`/v1/nodes/${folder}/children?limit=200`);
  if (!r.ok) {
    log("warn", "the runtime API answered {runtime_status}", { runtime_status: r.status });
    throw Object.assign(new Error("runtime API"), { code: `runtime_${r.status}` });
  }
  return (await r.json()).items ?? [];
}

async function handle(req, res) {
  const url = new URL(req.url ?? "/", "http://app.invalid");
  if (req.method !== "GET") return send(res, 405, "text/plain; charset=utf-8", "Read only.\n");
  const id = url.pathname === "/" ? FOLDER : url.pathname.slice("/folder/".length);
  if (url.pathname !== "/" && (!url.pathname.startsWith("/folder/") || !UUID.test(id))) {
    return send(res, 404, "text/plain; charset=utf-8", "Not found.\n");
  }
  const items = await children(id);
  const rows = items
    .map((c) =>
      c.kind === "folder" && UUID.test(String(c.node_id))
        ? `<li><a href="/folder/${c.node_id}">${escapeHtml(c.name)}/</a></li>`
        : `<li>${escapeHtml(c.name)} <small>${Number(c.size ?? 0)} bytes</small></li>`,
    )
    .join("\n");
  return send(
    res,
    200,
    "text/html; charset=utf-8",
    `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Files</title></head>
<body><h1>Files</h1><p><a href="/">Top</a></p><ul>${rows}</ul></body></html>`,
  );
}

const signedIn = withRuumUser(async (req, res) => {
  const started = Date.now();
  try {
    await handle(req, res);
  } catch (e) {
    log("error", "request failed with {error_code}", { error_code: e?.code ?? "unknown" });
    if (!res.headersSent) send(res, 502, "text/plain; charset=utf-8", "The files are not available now.\n");
  } finally {
    log("info", "{method} {path} answered {status} in {duration_ms} ms", {
      method: req.method,
      path: new URL(req.url ?? "/", "http://app.invalid").pathname.startsWith("/folder/") ? "/folder/:id" : "/",
      status: res.statusCode,
      duration_ms: Date.now() - started,
    });
  }
});

http
  .createServer((req, res) => {
    if (req.method === "GET" && req.url === "/healthz") return send(res, 200, "text/plain", "ok\n");
    return signedIn(req, res);
  })
  .listen(PORT, () => log("info", "listening on {port}", { port: PORT }));
