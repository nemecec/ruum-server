// Spike S-2 point 5: a hostile postinstall. First it tries to fill the job's output folder /out
// (96 MiB; run.sh gives /out a 64 MiB limit) and records where it stopped in /out/fill.json. Then
// it runs the probe (TCP connects to the metadata address, the platform Postgres and the other
// targets, the metadata GET, DNS, the Docker socket, the environment) from inside the build sandbox
// and writes what it reached to /out.
'use strict';
const fs = require('node:fs');

const chunk = Buffer.alloc(1 << 20, 0x61);
let written = 0;
let error = null;
try {
  const fd = fs.openSync('/out/fill', 'w');
  try {
    while (written < 96 * 2 ** 20) written += fs.writeSync(fd, chunk);
  } finally {
    fs.closeSync(fd);
  }
} catch (e) {
  error = e.code || e.message;
}
try {
  fs.unlinkSync('/out/fill');
} catch {}
fs.writeFileSync('/out/fill.json', JSON.stringify({ written, error }));

process.env.PROBE_OUT = process.env.PROBE_OUT || '/out/postinstall.json';
require(process.env.PROBE_JS || '/probe/probe.js');
