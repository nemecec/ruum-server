// The hostile postinstall of the builder's tests (card R-2b; decision 0006 G). It reads
// targets.json (written into the archive by tests/build.rs) and prints one line per target:
// `probe <json>`. It only opens TCP connections and closes them, sends one HTTP GET to the
// metadata address, resolves names and sends CONNECT requests to the egress proxy. It reads
// no data and sends no credentials. `ms` (installed from the npm registry through the proxy) is
// the positive control of egress itself.
'use strict';
const fs = require('node:fs');
const net = require('node:net');
const http = require('node:http');
const dns = require('node:dns').promises;
const os = require('node:os');

const T = 2000;
const report = (name, r) => process.stderr.write(`probe ${JSON.stringify({ name, ...r })}\n`);

function tcp(host, port) {
  return new Promise((resolve) => {
    const s = net.connect({ host, port, timeout: T });
    const done = (outcome, detail) => { s.destroy(); resolve({ reached: outcome === 'connected', outcome, detail }); };
    s.once('connect', () => done('connected', 'connected'));
    s.once('timeout', () => done('timeout', 'timeout'));
    s.once('error', (e) => done('error', e.code || e.message));
  });
}

function metadataGet() {
  return new Promise((resolve) => {
    const req = http.get({ host: '169.254.169.254', path: '/latest/meta-data/', timeout: T, agent: false }, (res) => {
      res.resume();
      resolve({ reached: true, outcome: 'answered', detail: `HTTP ${res.statusCode}` });
    });
    req.once('timeout', () => { req.destroy(); resolve({ reached: false, outcome: 'timeout', detail: 'timeout' }); });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
  });
}

// CONNECT through the egress proxy; the proxy's own refusal is 403 with X-Ruum-Egress-Deny.
function viaProxy(target) {
  const proxy = (process.env.HTTPS_PROXY || '').replace(/^http:\/\//, '');
  const i = proxy.lastIndexOf(':');
  return new Promise((resolve) => {
    const req = http.request({ host: proxy.slice(0, i), port: Number(proxy.slice(i + 1)), method: 'CONNECT', path: target, timeout: 8000, agent: false });
    const answer = (res, socket) => {
      if (socket) socket.destroy(); else res.resume();
      const deny = res.headers['x-ruum-egress-deny'] || null;
      resolve({ reached: res.statusCode === 200, status: res.statusCode, deny, outcome: res.statusCode === 200 ? 'tunnel' : deny ? 'denied' : 'proxy_status', detail: `proxy ${res.statusCode}${deny ? ` deny=${deny}` : ''}` });
    };
    req.once('connect', (res, socket) => answer(res, socket));
    req.once('response', (res) => answer(res, null));
    req.once('timeout', () => { req.destroy(); resolve({ reached: false, outcome: 'timeout', detail: 'timeout' }); });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
    req.end();
  });
}

// The would-be gateway of this network: the first address of its subnet.
function wouldBeGateway() {
  for (const list of Object.values(os.networkInterfaces())) {
    for (const a of list || []) {
      if (a.family !== 'IPv4' || a.internal) continue;
      const ip = a.address.split('.').map(Number);
      const mask = a.netmask.split('.').map(Number);
      const net4 = ip.map((b, k) => b & mask[k]);
      net4[3] += 1;
      return net4.join('.');
    }
  }
  return null;
}

async function main() {
  const cfg = JSON.parse(fs.readFileSync('targets.json', 'utf8'));
  report('egress control: ms installed', { kind: 'self', reached: fs.existsSync('node_modules/ms/index.js'), detail: 'node_modules/ms' });
  const gw = wouldBeGateway();
  for (const t of cfg.tcp || []) {
    const host = t.host === 'GATEWAY' ? gw : t.host;
    report(t.name, { kind: 'tcp', host, ...(host ? await tcp(host, t.port) : { reached: false, outcome: 'error', detail: 'no address' }) });
  }
  if (cfg.metadata) report('metadata HTTP GET', { kind: 'http', ...(await metadataGet()) });
  for (const name of cfg.dns || []) {
    try {
      const a = await dns.lookup(name);
      report(`DNS ${name}`, { kind: 'dns', reached: true, outcome: 'resolved', detail: a.address });
    } catch (e) {
      report(`DNS ${name}`, { kind: 'dns', reached: false, outcome: 'error', detail: e.code || e.message });
    }
  }
  for (const t of cfg.proxy || []) report(`proxy ${t}`, { kind: 'proxy', ...(await viaProxy(t)) });
  process.stderr.write('probe-done\n');
}

main();
