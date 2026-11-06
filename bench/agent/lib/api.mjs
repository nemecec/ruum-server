// A small REST client for the Ruum API, used by the setup steps, the reference solutions and the
// checkers. It never goes through MCP: the checker reads the final state through the API.

export class ApiError extends Error {
  constructor(method, path, status, body) {
    const code = body?.error?.code ?? 'unknown';
    super(`${method} ${path}: ${status} ${code}: ${body?.error?.message ?? JSON.stringify(body)}`);
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

export class Api {
  /** @param {string} base  e.g. http://127.0.0.1:8494 */
  constructor(base, token, tenant) {
    this.base = base;
    this.token = token;
    this.tenant = tenant;
  }

  async request(method, path, { json, raw, contentType, headers = {}, ok = [], text: asText = false } = {}) {
    const h = { Authorization: `Bearer ${this.token}`, ...headers };
    if (this.tenant) h['Ruum-Tenant'] = this.tenant;
    let body;
    if (json !== undefined) {
      h['Content-Type'] = 'application/json';
      body = JSON.stringify(json);
    } else if (raw !== undefined) {
      h['Content-Type'] = contentType ?? 'application/octet-stream';
      body = raw;
    }
    if (method !== 'GET' && method !== 'HEAD') h['Idempotency-Key'] = crypto.randomUUID();
    const res = await fetch(this.base + path, { method, headers: h, body });
    const text = await res.text();
    let data = text;
    if (!asText && (res.headers.get('content-type') ?? '').includes('json') && text) {
      data = JSON.parse(text);
    }
    if (!res.ok && !ok.includes(res.status)) throw new ApiError(method, path, res.status, data);
    return { status: res.status, data, headers: res.headers };
  }

  async get(path, opts) {
    return (await this.request('GET', path, opts)).data;
  }
  async post(path, json, opts = {}) {
    return (await this.request('POST', path, { ...opts, json })).data;
  }

  /** Every item of a paged list. */
  async all(path) {
    const items = [];
    let cursor = null;
    for (let i = 0; i < 100; i++) {
      const sep = path.includes('?') ? '&' : '?';
      const page = await this.get(`${path}${sep}limit=1000${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`);
      items.push(...(page.items ?? []));
      cursor = page.next_cursor;
      if (!cursor) break;
    }
    return items;
  }

  // ---------------------------------------------------------------- nodes
  root() {
    return this.get('/v1/nodes/root');
  }
  node(id) {
    return this.get(`/v1/nodes/${id}`);
  }
  /** The live node at a path from the tenant root, or null. */
  async byPath(path) {
    const enc = path.split('/').filter(Boolean).map(encodeURIComponent).join('/');
    const r = await this.request('GET', `/v1/nodes/by-path/${enc}`, { ok: [404] });
    return r.status === 404 ? null : r.data;
  }
  async mustPath(path) {
    const n = await this.byPath(path);
    if (!n) throw new Error(`no node at ${path}`);
    return n;
  }
  children(id) {
    return this.all(`/v1/nodes/${id}/children`);
  }
  async create(parentId, name, kind, type) {
    const body = { parent_id: parentId, name, kind };
    if (type) body.type = type;
    return (await this.post('/v1/nodes', body)).node;
  }
  async putContent(id, bytes, type) {
    return (await this.request('PUT', `/v1/nodes/${id}/content`, { raw: bytes, contentType: type })).data;
  }
  async content(id) {
    return (await this.request('GET', `/v1/nodes/${id}/content`, { text: true })).data;
  }
  versions(id) {
    return this.all(`/v1/nodes/${id}/versions`);
  }

  /** The path of a node from the root ("" for the root), walking parent_id. */
  async pathOf(id) {
    const names = [];
    let n = await this.node(id);
    for (let i = 0; i < 64 && n.parent_id; i++) {
      names.unshift(n.name);
      n = await this.node(n.parent_id);
    }
    return names.join('/');
  }

  // ---------------------------------------------------------------- tags, grants, links, agents
  tags() {
    return this.all('/v1/tags');
  }
  nodesByTag(tagId) {
    return this.all(`/v1/tags/${tagId}/nodes`);
  }
  grants(id) {
    return this.all(`/v1/nodes/${id}/grants`);
  }
  shareLinks(id) {
    return this.all(`/v1/nodes/${id}/share-links`);
  }
  agents() {
    return this.all('/v1/agents');
  }
}

/** Exchanges an agent API key for a short-lived access token (15 minutes). */
export async function exchangeKey(base, key) {
  const form = new URLSearchParams({
    grant_type: 'urn:ietf:params:oauth:grant-type:token-exchange',
    subject_token: key,
    subject_token_type: 'urn:ruum:params:oauth:token-type:api-key',
  });
  const res = await fetch(`${base}/v1/oauth/token`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: form,
  });
  const data = await res.json();
  if (!res.ok || !data.access_token) throw new Error(`token exchange: ${res.status} ${JSON.stringify(data)}`);
  return data.access_token;
}
