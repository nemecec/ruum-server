// Makes hostile output entries (card R-2b, backlog S-42): file names that a shell or tar would
// read as words or options, links out of the tree, a link inside it, a FIFO and a hard link.
'use strict';
const fs = require('node:fs');
const cp = require('node:child_process');
for (const n of ['-rf', '--checkpoint-action=exec=sh', 'a b', '$(touch pwned)', 'new\nline', 'back\\slash', '*']) {
  fs.writeFileSync(n, `content of ${JSON.stringify(n)}\n`);
}
fs.symlinkSync('/etc/passwd', 'escape');
fs.symlinkSync('../../../../etc', 'up');
fs.mkdirSync('deep');
fs.symlinkSync('../../outside', 'deep/up');
fs.symlinkSync('server.js', 'inside');
fs.writeFileSync('hl-a', 'hard linked\n');
fs.linkSync('hl-a', 'hl-b');
const r = cp.spawnSync('mkfifo', ['fifo']);
console.error(`ruum-fixture: names made (mkfifo ${r.status})`);
