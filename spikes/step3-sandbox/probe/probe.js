// Spike S-2: the probe of points 1 and 5. It runs inside a sandbox and tries to reach what an app
// (or a build) must never reach. It only opens TCP connections (and closes them at once), sends one
// HTTP GET to the metadata address, resolves names, and looks for the Docker socket. It records
// whether IPv6 is on inside the sandbox. It reads no data and sends no credentials.
//
// Input: PROBE_TARGETS (JSON [{name, host, port}]), EGRESS ("ip:port" of the egress proxy),
// EGRESS_ALLOWED and EGRESS_REFUSED (JSON ["host:port"]). PROBE_ONLY_TARGETS=1 runs the TCP targets
// only (a positive control from a container that must reach them).
// Output: one JSON object on stdout (or in PROBE_OUT): {name: {reached, outcome, detail, deny?}}.
//
// The probe only records what happened; run.sh decides. "Not reached" alone proves nothing (a
// target that is down looks the same), so run.sh pairs every TCP target with a positive control
// and accepts a refused egress request only with the proxy's own deny answer.
'use strict';
const net = require('node:net');
const http = require('node:http');
const tls = require('node:tls');
const fs = require('node:fs');
const dns = require('node:dns').promises;

const T = Number(process.env.PROBE_TIMEOUT_MS || 1500);
const DENY_HEADER = 'x-ruum-egress-deny';
const out = {};

// outcome: connected | refused | timeout | unreachable | error
function tcp(host, port) {
  return new Promise((resolve) => {
    const s = net.connect({ host, port, timeout: T });
    const done = (outcome, detail) => {
      s.destroy();
      resolve({ reached: outcome === 'connected', outcome, detail });
    };
    s.once('connect', () => done('connected', 'connected'));
    s.once('timeout', () => done('timeout', 'timeout'));
    s.once('error', (e) => {
      const c = e.code || e.message;
      const outcome = c === 'ECONNREFUSED' ? 'refused' : /UNREACH|EADDRNOTAVAIL/.test(c) ? 'unreachable' : 'error';
      done(outcome, c);
    });
  });
}

function metadataGet() {
  return new Promise((resolve) => {
    const req = http.get({ host: '169.254.169.254', path: '/latest/meta-data/', timeout: T }, (res) => {
      res.resume();
      resolve({ reached: true, outcome: 'connected', detail: `HTTP ${res.statusCode}` });
    });
    req.once('timeout', () => {
      req.destroy();
      resolve({ reached: false, outcome: 'timeout', detail: 'timeout' });
    });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
  });
}

// The proxy's own refusal: its status and the deny header with the reason. Anything else (502, a
// timeout, an error, a 403 without the header) is not a refusal by the proxy.
function proxyAnswer(res) {
  const deny = res.headers[DENY_HEADER];
  return {
    reached: false,
    outcome: deny ? 'denied' : 'proxy_status',
    detail: `proxy ${res.statusCode}${deny ? ` deny=${deny}` : ''}`,
    status: res.statusCode,
    deny: deny || null,
  };
}

// CONNECT through the egress proxy; after a 200 a TLS handshake and a HEAD request. A tunnel that
// the proxy opened counts as reached, whatever happens inside it.
function viaEgress(target) {
  const [ehost, eport] = (process.env.EGRESS || '').split(':');
  return new Promise((resolve) => {
    const req = http.request({ host: ehost, port: Number(eport), method: 'CONNECT', path: target, timeout: 5000 });
    req.once('connect', (res, socket) => {
      if (res.statusCode !== 200) {
        socket.destroy();
        resolve(proxyAnswer(res));
        return;
      }
      const servername = target.replace(/:\d+$/, '').replace(/^\[|\]$/g, '');
      const t = tls.connect({ socket, servername: net.isIP(servername) ? undefined : servername }, () => {
        t.write(`HEAD / HTTP/1.1\r\nHost: ${servername}\r\nConnection: close\r\n\r\n`);
      });
      let buf = '';
      t.setTimeout(5000, () => {
        t.destroy();
        resolve({ reached: true, outcome: 'tunnel', detail: 'proxy 200, tls timeout' });
      });
      t.on('data', (d) => {
        buf += d.toString();
        const m = buf.match(/^HTTP\/1\.[01] (\d+)/);
        if (m) {
          t.destroy();
          resolve({ reached: true, outcome: 'tunnel', detail: `proxy 200, HTTPS ${m[1]}`, https: Number(m[1]) });
        }
      });
      t.once('error', (e) => resolve({ reached: true, outcome: 'tunnel', detail: `proxy 200, tls ${e.code || e.message}` }));
    });
    req.once('response', (res) => {
      res.resume();
      resolve(proxyAnswer(res));
    });
    req.once('timeout', () => {
      req.destroy();
      resolve({ reached: false, outcome: 'timeout', detail: 'timeout' });
    });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
    req.end();
  });
}

function plainHttpViaEgress() {
  const [ehost, eport] = (process.env.EGRESS || '').split(':');
  return new Promise((resolve) => {
    const req = http.get({ host: ehost, port: Number(eport), path: 'http://example.com/', timeout: 5000 }, (res) => {
      res.resume();
      const a = proxyAnswer(res);
      if (res.statusCode === 200) Object.assign(a, { reached: true, outcome: 'forwarded' });
      resolve(a);
    });
    req.once('timeout', () => {
      req.destroy();
      resolve({ reached: false, outcome: 'timeout', detail: 'timeout' });
    });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
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
    ipv6: Object.fromEntries(
      ['all', 'default', 'eth0'].map((i) => {
        try {
          return [`disable_ipv6.${i}`, fs.readFileSync(`/proc/sys/net/ipv6/conf/${i}/disable_ipv6`, 'utf8').trim()];
        } catch (e) {
          return [`disable_ipv6.${i}`, e.code || 'unreadable'];
        }
      }),
    ),
  };
}

(async () => {
  const targets = JSON.parse(process.env.PROBE_TARGETS || '[]');
  await Promise.all(
    targets.map(async (t) => {
      out[t.name] = await tcp(t.host, t.port);
    }),
  );
  if (process.env.PROBE_ONLY_TARGETS !== '1') {
    out['metadata HTTP GET 169.254.169.254'] = await metadataGet();
    for (const name of ['registry.npmjs.org', 'example.com']) {
      try {
        const a = await dns.lookup(name);
        out[`DNS ${name}`] = { reached: true, outcome: 'resolved', detail: a.address };
      } catch (e) {
        out[`DNS ${name}`] = { reached: false, outcome: 'error', detail: e.code || e.message };
      }
    }
    if (process.env.EGRESS) {
      for (const t of JSON.parse(process.env.EGRESS_ALLOWED || '[]')) out[`egress allowed ${t}`] = await viaEgress(t);
      for (const t of JSON.parse(process.env.EGRESS_REFUSED || '[]')) out[`egress refused ${t}`] = await viaEgress(t);
      out['egress plain HTTP GET'] = await plainHttpViaEgress();
    }
    out.self = self();
  }
  const json = JSON.stringify(out);
  if (process.env.PROBE_OUT) fs.writeFileSync(process.env.PROBE_OUT, json);
  else process.stdout.write(json + '\n');
})();
