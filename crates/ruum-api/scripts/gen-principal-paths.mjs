// Generates crates/ruum-api/src/principal_paths.json from contracts/openapi.yaml, so the Rust
// crate needs no YAML library. The names middleware (src/names.rs) fills names only at these
// positions (backlog 2026-11-09 S-11): fields that the contract types as `PrincipalRef`, objects
// with `on_behalf_of_display_name`, and `GranteeUser`.
//
// Run from the repository root:
//   node crates/ruum-api/scripts/gen-principal-paths.mjs
//
// It needs `js-yaml` from web/node_modules (run `npm ci` in web/ first). Set JS_YAML to the path
// of another copy if needed. The test `names::tests::principal_paths_are_up_to_date` fails when
// the contract changes and this file was not run again (it compares the SHA-256 hash).
//
// A position is a list of steps from the top of a JSON answer: a property name, or `*` for every
// item of an array and every value of a map (`additionalProperties`). A field marked
// `x-ruum-content-trust: untrusted` is never entered: no name goes into data that others wrote.

import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '../../..');
const require = createRequire(import.meta.url);

function loadYamlLib() {
  const candidates = [process.env.JS_YAML, join(root, 'web/node_modules/js-yaml')].filter(Boolean);
  for (const c of candidates) {
    if (existsSync(c)) return require(c);
  }
  throw new Error('js-yaml not found: run npm ci in web/, or set JS_YAML');
}
const yaml = loadYamlLib();

const openapiText = readFileSync(join(root, 'contracts/openapi.yaml'), 'utf8');
const openapi = yaml.load(openapiText);
const sha = (s) => createHash('sha256').update(s, 'utf8').digest('hex');

/** Reads a JSON pointer (`#/a/b`) in the OpenAPI document. */
function pointer(ptr) {
  if (!ptr.startsWith('#/')) throw new Error(`not a local pointer: ${ptr}`);
  let v = openapi;
  for (const raw of ptr.slice(2).split('/')) {
    const key = raw.replace(/~1/g, '/').replace(/~0/g, '~');
    v = v?.[key];
    if (v === undefined) throw new Error(`pointer ${ptr} does not resolve`);
  }
  return v;
}

const MAX_DEPTH = 24;

/** Collects the positions under `schema` at `path` into `out`. */
function walk(schema, path, out, refs) {
  if (schema === null || typeof schema !== 'object') return;
  if (path.length > MAX_DEPTH) throw new Error(`schema too deep at ${path.join('.')}`);
  if (schema['x-ruum-content-trust'] === 'untrusted') return;
  if (schema.$ref) {
    const name = schema.$ref.split('/').pop();
    if (name === 'PrincipalRef') {
      out.refs.add(JSON.stringify(path));
      return;
    }
    if (name === 'GranteeUser') {
      out.grantees.add(JSON.stringify(path));
      return;
    }
    // A schema that holds itself (a tree): its own positions are the same below, so stop.
    if (refs.includes(schema.$ref)) return;
    walk(pointer(schema.$ref), path, out, [...refs, schema.$ref]);
  }
  for (const key of ['allOf', 'oneOf', 'anyOf']) {
    for (const alt of schema[key] ?? []) walk(alt, path, out, refs);
  }
  if (schema.properties) {
    if ('on_behalf_of_display_name' in schema.properties) out.obo.add(JSON.stringify(path));
    for (const [k, v] of Object.entries(schema.properties)) walk(v, [...path, k], out, refs);
  }
  if (schema.items) walk(schema.items, [...path, '*'], out, refs);
  if (schema.additionalProperties && typeof schema.additionalProperties === 'object') {
    walk(schema.additionalProperties, [...path, '*'], out, refs);
  }
}

const operations = [];
for (const [path, item] of Object.entries(openapi.paths)) {
  for (const [method, op] of Object.entries(item)) {
    if (!['get', 'put', 'post', 'delete', 'patch'].includes(method)) continue;
    const out = { refs: new Set(), obo: new Set(), grantees: new Set() };
    for (const [status, resp0] of Object.entries(op.responses ?? {})) {
      if (!/^2/.test(status)) continue;
      const resp = resp0.$ref ? pointer(resp0.$ref) : resp0;
      if (resp['x-ruum-content-trust'] === 'untrusted') continue;
      const media = resp.content?.['application/json'];
      if (!media?.schema) continue;
      if (media['x-ruum-content-trust'] === 'untrusted') continue;
      walk(media.schema, [], out, []);
    }
    const list = (s) => [...s].sort().map((p) => JSON.parse(p));
    if (out.refs.size + out.obo.size + out.grantees.size === 0) continue;
    operations.push({
      method: method.toUpperCase(),
      path,
      operation: op.operationId,
      refs: list(out.refs),
      obo: list(out.obo),
      grantees: list(out.grantees),
    });
  }
}
operations.sort((a, b) => (a.path + a.method).localeCompare(b.path + b.method));

const doc = { openapi_sha256: sha(openapiText), operations };
writeFileSync(join(root, 'crates/ruum-api/src/principal_paths.json'), JSON.stringify(doc, null, 1) + '\n');
console.log(`principal_paths.json: ${operations.length} operations`);
