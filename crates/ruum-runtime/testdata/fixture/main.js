// The fixture app of the Docker driver's tests (card R-2a). One image, two uses:
//
// 1. The conformance program (crates/ruum-core/src/runtime/program.rs): each argument is one
//    step, run in order. Each output step is one write(2), so a value split across two steps is
//    split across two writes.
// 2. Sandbox probes (tests/sandbox.rs): extra steps that look at the sandbox from inside and
//    print one line per result: `probe <json>`. They only open TCP connections and close them,
//    send UDP datagrams, send one HTTP GET to the metadata address, resolve names, and read
//    their own process state. They read no data and send no credentials.
'use strict';
const fs = require('node:fs');
const net = require('node:net');
const http = require('node:http');
const dgram = require('node:dgram');
const dns = require('node:dns').promises;
const crypto = require('node:crypto');
const cp = require('node:child_process');

const out = (s) => fs.writeSync(1, s);
const err = (s) => fs.writeSync(2, s);
const report = (name, r) => out(`probe ${JSON.stringify({ name, ...r })}\n`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const keep = [];
let kept = '';
const T = 1500;

function tokenText() {
  return fs.readFileSync(process.env.RUUM_TOKEN_FILE, 'utf8');
}

function serve(status) {
  const port = Number(process.env.PORT || 8080);
  http
    .createServer((req, res) => {
      res.writeHead(status, { 'content-type': 'text/plain' });
      res.end(status === 200 ? 'ok' : 'unhealthy');
    })
    .listen(port, '0.0.0.0');
  return new Promise(() => {});
}

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
      done(c === 'ECONNREFUSED' ? 'refused' : /UNREACH|EADDRNOTAVAIL/.test(c) ? 'unreachable' : 'error', c);
    });
  });
}

// A datagram to host:port; reached only when the echo comes back.
function udp(host, port) {
  return new Promise((resolve) => {
    const s = dgram.createSocket(net.isIPv6(host.split('%')[0]) ? 'udp6' : 'udp4');
    const payload = Buffer.from(`ruum-udp-${crypto.randomUUID()}`);
    const done = (r) => {
      clearTimeout(timer);
      try { s.close(); } catch {}
      resolve(r);
    };
    const timer = setTimeout(() => done({ reached: false, outcome: 'timeout', detail: 'no echo' }), T);
    s.on('message', (m) => done(m.equals(payload)
      ? { reached: true, outcome: 'echo', detail: 'echo' }
      : { reached: false, outcome: 'other', detail: 'other datagram' }));
    s.on('error', (e) => done({ reached: false, outcome: 'error', detail: e.code || e.message }));
    s.send(payload, port, host, (e) => {
      if (e) done({ reached: false, outcome: 'error', detail: e.code || e.message });
    });
  });
}

function metadataGet() {
  return new Promise((resolve) => {
    const req = http.get({ host: '169.254.169.254', path: '/latest/meta-data/', timeout: T }, (res) => {
      res.resume();
      resolve({ reached: true, outcome: 'connected', detail: `HTTP ${res.statusCode}` });
    });
    req.once('timeout', () => { req.destroy(); resolve({ reached: false, outcome: 'timeout', detail: 'timeout' }); });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
  });
}

// CONNECT through the egress proxy; the proxy's own refusal is 403 with X-Ruum-Egress-Deny.
function viaProxy(proxy, target) {
  const [phost, pport] = proxy.replace(/^http:\/\//, '').split(':');
  return new Promise((resolve) => {
    const req = http.request({ host: phost, port: Number(pport), method: 'CONNECT', path: target, timeout: 8000 });
    req.once('connect', (res, socket) => {
      socket.destroy();
      const deny = res.headers['x-ruum-egress-deny'] || null;
      resolve({ reached: res.statusCode === 200, outcome: res.statusCode === 200 ? 'tunnel' : deny ? 'denied' : 'proxy_status', status: res.statusCode, deny, detail: `proxy ${res.statusCode}${deny ? ` deny=${deny}` : ''}` });
    });
    req.once('response', (res) => {
      res.resume();
      const deny = res.headers['x-ruum-egress-deny'] || null;
      resolve({ reached: false, outcome: deny ? 'denied' : 'proxy_status', status: res.statusCode, deny, detail: `proxy ${res.statusCode}${deny ? ` deny=${deny}` : ''}` });
    });
    req.once('timeout', () => { req.destroy(); resolve({ reached: false, outcome: 'timeout', detail: 'timeout' }); });
    req.once('error', (e) => resolve({ reached: false, outcome: 'error', detail: e.code || e.message }));
    req.end();
  });
}

function procStatus() {
  const st = fs.readFileSync('/proc/self/status', 'utf8');
  const field = (k) => (st.match(new RegExp(`^${k}:\\s*(.*)$`, 'm')) || [])[1] || null;
  return { cap_eff: field('CapEff'), cap_bnd: field('CapBnd'), no_new_privs: field('NoNewPrivs'), seccomp: field('Seccomp') };
}

function tryWrite(path) {
  try {
    fs.writeFileSync(path, 'x');
    fs.unlinkSync(path);
    return 'written';
  } catch (e) {
    return e.code || e.message;
  }
}

function fillTmp(limit) {
  const chunk = Buffer.alloc(1 << 20, 7);
  let written = 0;
  let fd;
  try {
    fd = fs.openSync('/tmp/fill', 'w');
    while (written < limit) {
      written += fs.writeSync(fd, chunk);
    }
    return { written, error: null };
  } catch (e) {
    return { written, error: e.code || e.message };
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
    try { fs.unlinkSync('/tmp/fill'); } catch {}
  }
}

async function forkBomb(n, ms) {
  const kids = [];
  const errs = {};
  let spawned = 0;
  for (let i = 0; i < n; i++) {
    try {
      const k = cp.spawn('sleep', [String(Math.ceil(ms / 1000) + 5)], { stdio: 'ignore' });
      k.on('error', (e) => { errs[e.code] = (errs[e.code] || 0) + 1; });
      kids.push(k);
      spawned++;
    } catch (e) {
      errs[`sync_${e.code}`] = (errs[`sync_${e.code}`] || 0) + 1;
      break;
    }
  }
  await sleep(ms);
  const alive = kids.filter((k) => k.pid && k.exitCode === null && k.signalCode === null).length;
  kids.forEach((k) => { try { k.kill('SIGKILL'); } catch {} });
  return { tried: n, spawned, alive, errs };
}

function run(cmd, args) {
  const r = cp.spawnSync(cmd, args, { encoding: 'utf8', timeout: 5000 });
  return { status: r.status, error: r.error ? r.error.code : null, stderr: (r.stderr || '').slice(0, 200) };
}

async function probe(cfg) {
  for (const t of cfg.tcp || []) report(t.name, { kind: 'tcp', ...(await tcp(t.host, t.port)) });
  for (const t of cfg.udp || []) report(t.name, { kind: 'udp', ...(await udp(t.host, t.port)) });
  if (cfg.metadata) report('metadata HTTP GET', { kind: 'http', ...(await metadataGet()) });
  for (const name of cfg.dns || []) {
    try {
      const a = await dns.lookup(name);
      report(`DNS ${name}`, { kind: 'dns', reached: true, outcome: 'resolved', detail: a.address });
    } catch (e) {
      report(`DNS ${name}`, { kind: 'dns', reached: false, outcome: 'error', detail: e.code || e.message });
    }
  }
  for (const t of cfg.proxy || []) report(`proxy ${t}`, { kind: 'proxy', ...(await viaProxy(process.env.HTTPS_PROXY || '', t)) });
  if (cfg.fetch) {
    for (const u of cfg.fetch) {
      try {
        const r = await fetch(u, { signal: AbortSignal.timeout(10000) });
        report(`fetch ${u}`, { kind: 'fetch', reached: true, outcome: 'answered', status: r.status, detail: `HTTP ${r.status}` });
      } catch (e) {
        const c = (e.cause && (e.cause.code || e.cause.message)) || e.code || e.message;
        report(`fetch ${u}`, { kind: 'fetch', reached: false, outcome: 'error', detail: String(c) });
      }
    }
  }
  if (cfg.self) {
    let ipv6 = null;
    try { ipv6 = fs.readFileSync('/proc/sys/net/ipv6/conf/eth0/disable_ipv6', 'utf8').trim(); } catch (e) { ipv6 = e.code; }
    let rootMount = null;
    try { rootMount = (fs.readFileSync('/proc/mounts', 'utf8').split('\n').find((l) => l.split(' ')[1] === '/') || '').split(' ')[3] || null; } catch {}
    report('self', {
      kind: 'self',
      uid: process.getuid(),
      gid: process.getgid(),
      groups: process.getgroups(),
      ...procStatus(),
      root_mount: rootMount,
      root_write: tryWrite('/probe-home/x'),
      etc_write: tryWrite('/etc/ruum-probe'),
      tmp: fillTmp(cfg.self.tmp_limit),
      docker_socket: ['/var/run/docker.sock', '/run/docker.sock'].filter((p) => fs.existsSync(p)),
      env_keys: Object.keys(process.env).sort(),
      disable_ipv6: ipv6,
      mount: run('mount', ['-t', 'tmpfs', 'none', '/tmp']),
      unshare: run('unshare', ['-U', 'true']),
      gvisor: (() => { try { return /gVisor/.test(cp.execFileSync('dmesg', { encoding: 'utf8', timeout: 3000 })); } catch { return false; } })(),
    });
  }
  if (cfg.forkbomb) report('forkbomb', { kind: 'forkbomb', ...(await forkBomb(cfg.forkbomb.n, cfg.forkbomb.ms)) });
}

function udpEcho(port) {
  const s = dgram.createSocket('udp4');
  s.on('message', (m, r) => s.send(m, r.port, r.address));
  s.bind(port, '0.0.0.0');
  keep.push(s);
}

async function main(args) {
  let last = '';
  for (const arg of args) {
    const i = arg.indexOf(':');
    const verb = i < 0 ? arg : arg.slice(0, i);
    const rest = i < 0 ? null : arg.slice(i + 1);
    last = verb;
    switch (verb) {
      case 'print': out(`${rest}\n`); break;
      case 'write': out(rest); break;
      case 'stderr': err(`${rest}\n`); break;
      case 'stderr-write': err(rest); break;
      case 'env': out(`${rest}=${process.env[rest] ?? ''}\n`); break;
      case 'token': out(`${tokenText()}\n`); break;
      case 'token-sha': out(`token-sha:${crypto.createHash('sha256').update(tokenText()).digest('hex').slice(0, 16)}\n`); break;
      case 'sleep': await sleep(Number(rest)); break;
      case 'exit': process.exit(Number(rest));
      // falls through (never reached)
      case 'serve': return serve(200);
      case 'unhealthy': return serve(500);
      case 'alloc': keep.push(Buffer.alloc(Number(rest), 1)); break;
      case 'disk': {
        const n = Number(rest);
        const chunk = Buffer.alloc(1 << 20, 1);
        const fd = fs.openSync(`/tmp/disk-${crypto.randomUUID()}`, 'w');
        let left = n;
        try {
          while (left > 0) left -= fs.writeSync(fd, chunk, 0, Math.min(left, chunk.length));
        } catch (e) {
          if (e.code === 'ENOSPC') { out('disk:full\n'); process.exit(1); }
          throw e;
        } finally {
          fs.closeSync(fd);
        }
        break;
      }
      // Sandbox steps (not part of the conformance program).
      case 'dump-env':
        for (const k of Object.keys(process.env).sort()) out(`${k}=${process.env[k]}\n`);
        break;
      case 'encoded': {
        const v = Buffer.from(process.env[rest] ?? '');
        out(`hex:${v.toString('hex')}\n`);
        out(`b64:${v.toString('base64')}\n`);
        break;
      }
      case 'token-keep': kept = tokenText(); break;
      case 'token-kept': out(`kept=${kept}\n`); break;
      case 'udp-echo': udpEcho(Number(rest)); break;
      case 'probe': await probe(JSON.parse(rest)); out('probe-done\n'); break;
      case 'hold': return new Promise(() => {});
      default: err(`unknown step ${verb}\n`); process.exit(64);
    }
  }
  if (last !== 'exit') process.exit(0);
}

main(process.argv.slice(2));
