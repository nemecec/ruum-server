// Spike S-2: the probe of points 1 and 5. It runs inside a sandbox and tries to reach what an app
// (or a build) must never reach. It only opens TCP connections (and closes them at once), sends one
// HTTP GET to the metadata address, resolves names, and looks for the Docker socket. It reads no
// data and sends no credentials. Input: PROBE_TARGETS (JSON [{name, host, port}]), EGRESS
// ("ip:port" of the egress proxy), EGRESS_ALLOWED and EGRESS_REFUSED (JSON ["host:port"]).
// Output: one JSON object on stdout (or in PROBE_OUT): {name: {reached: bool, detail}}.
'use strict';
const net = require('node:net');
const http = require('node:http');
const tls = require('node:tls');
const fs = require('node:fs');
const dns = require('node:dns').promises;

const T = Number(process.env.PROBE_TIMEOUT_MS || 1500);
const out = {};

function tcp(host, port) {
  return new Promise((resolve) => {
    const s = net.connect({ host, port, timeout: T });
    const done = (reached, detail) => {
      s.destroy();
      resolve({ reached, detail });
    };
    s.once('connect', () => done(true, 'connected'));
    s.once('timeout', () => done(false, 'timeout'));
    s.once('error', (e) => done(false, e.code || e.message));
  });
}

function metadataGet() {
  return new Promise((resolve) => {
    const req = http.get({ host: '169.254.169.254', path: '/latest/meta-data/', timeout: T }, (res) => {
      res.resume();
      resolve({ reached: true, detail: `HTTP ${res.statusCode}` });
    });
    req.once('timeout', () => {
      req.destroy();
      resolve({ reached: false, detail: 'timeout' });
    });
    req.once('error', (e) => resolve({ reached: false, detail: e.code || e.message }));
  });
}

// CONNECT through the egress proxy; for an allowed target also a TLS handshake and a HEAD request.
function viaEgress(target) {
  const [ehost, eport] = (process.env.EGRESS || '').split(':');
  return new Promise((resolve) => {
    const req = http.request({ host: ehost, port: Number(eport), method: 'CONNECT', path: target, timeout: 5000 });
    req.once('connect', (res, socket) => {
      if (res.statusCode !== 200) {
        socket.destroy();
        resolve({ reached: false, detail: `proxy ${res.statusCode}` });
        return;
      }
      const servername = target.split(':')[0];
      const t = tls.connect({ socket, servername }, () => {
        t.write(`HEAD / HTTP/1.1\r\nHost: ${servername}\r\nConnection: close\r\n\r\n`);
      });
      let buf = '';
      t.setTimeout(5000, () => {
        t.destroy();
        resolve({ reached: false, detail: 'tls timeout' });
      });
      t.on('data', (d) => {
        buf += d.toString();
        const m = buf.match(/^HTTP\/1\.[01] (\d+)/);
        if (m) {
          t.destroy();
          resolve({ reached: true, detail: `HTTPS ${m[1]}` });
        }
      });
      t.once('error', (e) => resolve({ reached: false, detail: `tls ${e.code || e.message}` }));
    });
    req.once('response', (res) => resolve({ reached: false, detail: `proxy ${res.statusCode}` }));
    req.once('timeout', () => {
      req.destroy();
      resolve({ reached: false, detail: 'timeout' });
    });
    req.once('error', (e) => resolve({ reached: false, detail: e.code || e.message }));
    req.end();
  });
}

function plainHttpViaEgress() {
  const [ehost, eport] = (process.env.EGRESS || '').split(':');
  return new Promise((resolve) => {
    const req = http.get({ host: ehost, port: Number(eport), path: 'http://example.com/', timeout: 5000 }, (res) => {
      res.resume();
      resolve({ reached: res.statusCode === 200, detail: `proxy ${res.statusCode}` });
    });
    req.once('timeout', () => {
      req.destroy();
      resolve({ reached: false, detail: 'timeout' });
    });
    req.once('error', (e) => resolve({ reached: false, detail: e.code || e.message }));
  });
}

function self() {
  const st = fs.readFileSync('/proc/self/status', 'utf8');
  const field = (k) => (st.match(new RegExp(`^${k}:\\s*(.*)$`, 'm')) || [])[1];
  let rootWritable = false;
  try {
    fs.writeFileSync('/probe-write-test', 'x');
    rootWritable = true;
    fs.unlinkSync('/probe-write-test');
  } catch {}
  return {
    uid: process.getuid(),
    cap_eff: field('CapEff'),
    cap_bnd: field('CapBnd'),
    no_new_privs: field('NoNewPrivs'),
    seccomp: field('Seccomp'),
    root_writable: rootWritable,
    docker_socket: ['/var/run/docker.sock', '/run/docker.sock'].some((p) => fs.existsSync(p)),
    env_keys: Object.keys(process.env).filter((k) => /SECRET|TOKEN|PASSWORD|KEY|DATABASE_URL/i.test(k)),
  };
}

(async () => {
  const targets = JSON.parse(process.env.PROBE_TARGETS || '[]');
  await Promise.all(
    targets.map(async (t) => {
      out[t.name] = await tcp(t.host, t.port);
    }),
  );
  out['metadata HTTP GET 169.254.169.254'] = await metadataGet();
  for (const name of ['registry.npmjs.org', 'example.com']) {
    try {
      const a = await dns.lookup(name);
      out[`DNS ${name}`] = { reached: true, detail: a.address };
    } catch (e) {
      out[`DNS ${name}`] = { reached: false, detail: e.code || e.message };
    }
  }
  if (process.env.EGRESS) {
    for (const t of JSON.parse(process.env.EGRESS_ALLOWED || '[]')) out[`egress allowed ${t}`] = await viaEgress(t);
    for (const t of JSON.parse(process.env.EGRESS_REFUSED || '[]')) out[`egress refused ${t}`] = await viaEgress(t);
    out['egress plain HTTP GET'] = await plainHttpViaEgress();
  }
  out.self = self();
  const json = JSON.stringify(out);
  if (process.env.PROBE_OUT) fs.writeFileSync(process.env.PROBE_OUT, json);
  else process.stdout.write(json + '\n');
})();
