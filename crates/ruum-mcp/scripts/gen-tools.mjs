// Generates crates/ruum-mcp/src/tools.json from contracts/mcp/tools.yaml and
// contracts/openapi.yaml, so the Rust crate needs no YAML library.
//
// Run from the repository root:
//   node crates/ruum-mcp/scripts/gen-tools.mjs
//
// It needs `js-yaml` from web/node_modules (run `npm ci` in web/ first). Set JS_YAML to the path
// of another copy if needed. The test `tools_json_is_up_to_date` fails when the contract changes
// and this file was not run again (it compares SHA-256 hashes of both YAML files).

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
const toolsText = readFileSync(join(root, 'contracts/mcp/tools.yaml'), 'utf8');
const openapi = yaml.load(openapiText);
const toolsDoc = yaml.load(toolsText);
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

/** Inlines every `$ref`, drops OpenAPI-only keys. */
function resolveSchema(s, seen = []) {
  if (Array.isArray(s)) return s.map((x) => resolveSchema(x, seen));
  if (s === null || typeof s !== 'object') return s;
  if (s.$ref) {
    if (seen.includes(s.$ref)) throw new Error(`recursive schema ${s.$ref}`);
    const target = resolveSchema(pointer(s.$ref), [...seen, s.$ref]);
    const rest = { ...s };
    delete rest.$ref;
    return Object.keys(rest).length ? { ...target, ...resolveSchema(rest, seen) } : target;
  }
  const out = {};
  for (const [k, v] of Object.entries(s)) {
    if (k === 'example' || k === 'examples' || k.startsWith('x-') || k === 'xml') continue;
    if (k === 'properties') {
      out[k] = Object.fromEntries(Object.entries(v).map(([pk, pv]) => [pk, resolveSchema(pv, seen)]));
    } else {
      out[k] = resolveSchema(v, seen);
    }
  }
  return out;
}

const SKIP_HEADERS = new Set(['ruum-tenant', 'idempotency-key']);
const snake = (h) => h.toLowerCase().replace(/-/g, '_');

const tools = [];
for (const t of toolsDoc.tools) {
  const ptr = t.input;
  const m = /^#\/paths\/(.+)\/(get|put|post|delete|patch)$/.exec(ptr);
  if (!m) throw new Error(`tool ${t.name}: bad input pointer ${ptr}`);
  const path = m[1].replace(/~1/g, '/').replace(/~0/g, '~');
  const method = m[2];
  const pathItem = openapi.paths[path];
  const op = pathItem[method];
  if (op.operationId !== t.operation) {
    throw new Error(`tool ${t.name}: ${ptr} is ${op.operationId}, not ${t.operation}`);
  }
  const params = [];
  const properties = {};
  const required = [];
  const all = [...(pathItem.parameters ?? []), ...(op.parameters ?? [])].map((p) =>
    p.$ref ? pointer(p.$ref) : p,
  );
  // Operation parameters override path-level ones with the same name and place.
  const byKey = new Map();
  for (const p of all) byKey.set(`${p.in}:${p.name.toLowerCase()}`, p);
  for (const p of byKey.values()) {
    if (p.in === 'header' && SKIP_HEADERS.has(p.name.toLowerCase())) continue;
    if (p.in === 'cookie') continue;
    const prop = p.in === 'header' ? snake(p.name) : p.name;
    const schema = resolveSchema(p.schema ?? { type: 'string' });
    if (p.description && !schema.description) schema.description = p.description.trim();
    properties[prop] = schema;
    if (p.required) required.push(prop);
    params.push({ name: p.name, in: p.in, prop });
  }
  let body = null;
  const rb = op.requestBody ? (op.requestBody.$ref ? pointer(op.requestBody.$ref) : op.requestBody) : null;
  if (rb) {
    const content = rb.content ?? {};
    if (t.name === 'write_file') {
      body = 'content';
      properties.content = { type: 'string', description: 'The new content of the file.' };
      properties.encoding = {
        type: 'string',
        enum: ['utf8', 'base64'],
        default: 'utf8',
        description: '`utf8` for text, `base64` for binary content.',
      };
      required.push('content');
    } else if (content['application/json']) {
      body = 'json';
      properties.body = resolveSchema(content['application/json'].schema ?? { type: 'object' });
      if (rb.required) required.push('body');
    } else {
      throw new Error(`tool ${t.name}: request body type ${Object.keys(content)} is not handled`);
    }
  }
  const input_schema = { type: 'object', properties, additionalProperties: false };
  if (required.length) input_schema.required = required;
  tools.push({
    name: t.name,
    operation: t.operation,
    // Specified but not built (decision 0004-22): the server does not offer the tool yet.
    planned: op['x-ruum-status'] === 'planned',
    description: t.description,
    notes: t.notes ?? null,
    method: method.toUpperCase(),
    path,
    params,
    body,
    input_schema,
  });
}

const out = {
  generated_by: 'crates/ruum-mcp/scripts/gen-tools.mjs',
  openapi_sha256: sha(openapiText),
  tools_sha256: sha(toolsText),
  tools,
};
writeFileSync(join(root, 'crates/ruum-mcp/src/tools.json'), JSON.stringify(out, null, 1) + '\n');
console.log(`wrote ${tools.length} tools`);
