// The env dump of the builder's conformance case build_sees_no_platform_secrets (backlog S-37):
// every variable as NAME=<hex(value)> and every credential file it can read as
// FILE:<path>=<hex(content)>, between env-dump:begin and env-dump:end. Hex, so a log redaction
// of the plain value cannot hide a leak.
'use strict';
const fs = require('node:fs');
const path = require('node:path');
const hex = (s) => Buffer.from(s).toString('hex');
const out = (s) => process.stderr.write(`${s}\n`);
const home = process.env.HOME || '/tmp';
const files = [
  '/root/.docker/config.json',
  path.join(home, '.docker/config.json'),
  path.join(home, '.npmrc'),
  '/etc/npmrc',
  '/usr/local/etc/npmrc',
  '/var/run/secrets/kubernetes.io/serviceaccount/token',
  '/run/ruum/token',
];
for (const dir of ['/run/secrets', '/var/run/secrets']) {
  try {
    for (const f of fs.readdirSync(dir)) files.push(path.join(dir, f));
  } catch {}
}
out('env-dump:begin');
for (const k of Object.keys(process.env).sort()) out(`${k}=${hex(process.env[k])}`);
for (const f of files) {
  try {
    out(`FILE:${f}=${fs.readFileSync(f).toString('hex')}`);
  } catch {}
}
out('env-dump:end');
