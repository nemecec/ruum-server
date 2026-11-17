// Starts 2000 processes (card R-2b: the pids limit stops a fork bomb). Prints how many started
// and the errors, then exits 1, so the build fails.
'use strict';
const cp = require('node:child_process');
const kids = [];
const errs = {};
let spawned = 0;
for (let i = 0; i < 2000; i++) {
  try {
    const k = cp.spawn('sleep', ['30'], { stdio: 'ignore' });
    k.on('error', (e) => { errs[e.code] = (errs[e.code] || 0) + 1; });
    kids.push(k);
    spawned++;
  } catch (e) {
    errs[`sync_${e.code}`] = (errs[`sync_${e.code}`] || 0) + 1;
  }
}
setTimeout(() => {
  const alive = kids.filter((k) => k.pid && k.exitCode === null && k.signalCode === null).length;
  console.error(`ruum-fixture: forkbomb ${JSON.stringify({ tried: 2000, spawned, alive, errs })}`);
  kids.forEach((k) => { try { k.kill('SIGKILL'); } catch {} });
  process.exit(1);
}, 3000);
