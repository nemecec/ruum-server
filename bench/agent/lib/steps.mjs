// The step language of the task files: `setup` (the state before the agent runs) and `reference`
// (a correct solution through the REST API, used by `--mode oracle` to prove that a checker
// accepts a right answer).
//
// Every step is a map with one action key and optional `as` (the principal that does it:
// `alice` by default, `bob`, `bench` for the benchmark agent, or the name of an agent that an
// `agent` step registered). Paths are from the tenant root; Alice (the owner) resolves them, so
// a step can name a node that its principal reaches only by ID.
//
// Databases (step 2): `database: <path>` makes a database node (`engine`: sqlite by default) and
// waits until it is ready; `sql: <statement>` runs one statement on a database (`node: <saved>`
// or `db: <path>`, `params`, `mode`: write by default) through `queryDatabase`. A person with
// `manage` (Alice) makes tables this way (Q-1 wave decision 3). `answer: <text>` is the final
// answer of a reference solution (`--mode oracle`), for the `answer_*` checks.

import { Api, exchangeKey } from './api.mjs';

/** Replaces ${NAME} with the task variables (seed values and derived names). */
export function template(value, vars) {
  if (typeof value === 'string') {
    return value.replace(/\$\{([A-Z0-9_]+)\}/g, (m, k) => (k in vars ? vars[k] : m));
  }
  if (Array.isArray(value)) return value.map((v) => template(v, vars));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, template(v, vars)]));
  }
  return value;
}

const ACTIONS = [
  'folder', 'file', 'write', 'label', 'agent', 'grant', 'tag', 'attach', 'rename', 'move',
  'trash', 'restore', 'restore_version', 'sleep', 'database', 'sql', 'answer',
];

export function actionOf(step) {
  const found = ACTIONS.filter((a) => a in step);
  if (found.length !== 1) throw new Error(`a step needs exactly one of ${ACTIONS.join(', ')}: ${JSON.stringify(step)}`);
  return found[0];
}

function split(path) {
  const parts = path.split('/').filter(Boolean);
  return { parent: parts.slice(0, -1).join('/'), name: parts.at(-1) };
}

/** The node ID a step names: `node: <saved name>` or a path. */
async function target(ctx, step, key) {
  if (step.node) {
    const id = ctx.saved[step.node];
    if (!id) throw new Error(`no saved node ${step.node}`);
    return id;
  }
  return (await ctx.inspector.mustPath(step[key])).id;
}

/** mkdir -p as the principal; returns the folder ID. */
export async function ensureFolder(ctx, api, path) {
  let parent = (await ctx.inspector.root()).id;
  let sofar = '';
  for (const name of path.split('/').filter(Boolean)) {
    sofar = sofar ? `${sofar}/${name}` : name;
    const found = await ctx.inspector.byPath(sofar);
    parent = found ? found.id : (await api.create(parent, name, 'folder')).id;
  }
  return parent;
}

function bytesOf(step) {
  if (step.base64 !== undefined) return Buffer.from(step.base64, 'base64');
  return Buffer.from(step.content ?? '', 'utf8');
}

async function tagId(ctx, api, name) {
  const tags = await ctx.inspector.tags();
  const t = tags.find((x) => x.name === name);
  if (t) return t.id;
  return (await api.post('/v1/tags', { name })).tag?.id ?? (await ctx.inspector.tags()).find((x) => x.name === name).id;
}

export async function runStep(ctx, raw) {
  const step = template(raw, ctx.vars);
  const action = actionOf(step);
  const who = step.as ?? 'alice';
  const api = ctx.principals[who];
  if (!api && action !== 'sleep') throw new Error(`unknown principal ${who}`);
  switch (action) {
    case 'sleep':
      await new Promise((r) => setTimeout(r, step.sleep * 1000));
      return;
    case 'folder': {
      const id = await ensureFolder(ctx, api, step.folder);
      if (step.save) ctx.saved[step.save] = id;
      return;
    }
    case 'file': {
      const { parent, name } = split(step.file);
      const pid = parent ? await ensureFolder(ctx, api, parent) : (await ctx.inspector.root()).id;
      const type = step.type ?? 'text/plain';
      const node = await api.create(pid, name, 'file', type);
      await api.putContent(node.id, bytesOf(step), type);
      if (step.save) ctx.saved[step.save] = node.id;
      return;
    }
    case 'write': {
      const id = await target(ctx, step, 'write');
      const n = await ctx.inspector.node(id);
      await api.putContent(id, bytesOf(step), n.type ?? 'text/plain');
      return;
    }
    case 'label': {
      const id = await target(ctx, step, 'label');
      const n = await ctx.inspector.node(id);
      await api.request('PUT', `/v1/nodes/${id}/versions/${n.head_version}/label`, { json: { label: step.text } });
      return;
    }
    case 'agent': {
      // Registers an agent for the principal, makes a key and exchanges it for a token.
      const scope = { subtree: null, max_role: 'manage', ...(step.scope ?? {}) };
      if (scope.subtree_path) {
        scope.subtree = (await ctx.inspector.mustPath(scope.subtree_path)).id;
        delete scope.subtree_path;
      }
      const agent = await api.post('/v1/agents', { name: step.agent, description: step.description ?? 'agent benchmark', scope });
      const key = await api.post(`/v1/agents/${agent.id}/keys`, { name: 'bench' });
      const token = await exchangeKey(ctx.base, key.secret);
      ctx.principals[step.save ?? step.agent] = new Api(ctx.base, token, ctx.vars.TENANT_A);
      ctx.agents[step.save ?? step.agent] = { id: agent.id, name: step.agent, owner: who };
      return;
    }
    case 'grant': {
      const id = await target(ctx, step, 'grant');
      let grantee;
      if (step.to === 'bob') grantee = { type: 'user', id: ctx.vars.BOB_ID };
      else if (step.to?.email) grantee = { type: 'email', email: step.to.email };
      else if (step.to?.user) grantee = { type: 'user', id: step.to.user };
      else throw new Error(`grant: unknown grantee ${JSON.stringify(step.to)}`);
      await api.post(`/v1/nodes/${id}/grants`, { grantee, role: step.role, notify: false });
      return;
    }
    case 'tag':
      await tagId(ctx, api, step.tag);
      return;
    case 'attach': {
      const id = await target(ctx, step, 'attach');
      const t = await tagId(ctx, api, step.tag_name);
      await api.request('PUT', `/v1/nodes/${id}/tags/${t}`);
      return;
    }
    case 'rename': {
      const id = await target(ctx, step, 'rename');
      await api.post(`/v1/nodes/${id}/rename`, { name: step.name });
      return;
    }
    case 'move': {
      const id = await target(ctx, step, 'move');
      const to = await ensureFolder(ctx, api, step.to);
      const body = { parent_id: to };
      if (step.name) body.name = step.name;
      if (step.on_conflict) body.on_conflict = step.on_conflict;
      await api.post(`/v1/nodes/${id}/move`, body);
      return;
    }
    case 'trash': {
      const id = await target(ctx, step, 'trash');
      await api.post(`/v1/nodes/${id}/trash`, {});
      return;
    }
    case 'restore': {
      const id = await target(ctx, step, 'restore');
      await api.post(`/v1/nodes/${id}/restore`, {});
      return;
    }
    case 'database': {
      const { parent, name } = split(step.database);
      const pid = parent ? await ensureFolder(ctx, api, parent) : (await ctx.inspector.root()).id;
      const engine = step.engine ?? 'sqlite';
      const manifest = { engine, version: engine === 'postgres' ? '17' : '3', plan: 'shared-small' };
      const w = await api.post('/v1/databases', { parent_id: pid, name, manifest });
      const id = w.database.node.id;
      for (let i = 0; ; i++) {
        const state = (await api.get(`/v1/databases/${id}`)).status.state;
        if (state === 'ready') break;
        if (!['provisioning', 'updating'].includes(state) || i >= 600) {
          throw new Error(`database ${step.database}: state ${state}`);
        }
        await new Promise((r) => setTimeout(r, 100));
      }
      if (step.save) ctx.saved[step.save] = id;
      return;
    }
    case 'sql': {
      const id = await target(ctx, step, 'db');
      await api.post(`/v1/databases/${id}/query`, {
        sql: step.sql,
        params: step.params ?? [],
        mode: step.mode ?? 'write',
      });
      return;
    }
    case 'answer':
      ctx.answer = step.answer;
      return;
    case 'restore_version': {
      const id = await target(ctx, step, 'restore_version');
      const versions = (await ctx.inspector.versions(id)).sort((a, b) => a.created_at.localeCompare(b.created_at));
      const v = versions[step.version - 1];
      if (!v) throw new Error(`restore_version: no version ${step.version} (have ${versions.length})`);
      await api.post(`/v1/nodes/${id}/versions/${v.id}/restore`, {});
      return;
    }
    default:
      throw new Error(`unhandled action ${action}`);
  }
}
