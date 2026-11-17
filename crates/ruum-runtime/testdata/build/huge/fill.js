// Writes size.txt MiB into the build's work folder (card R-2b: the output limit).
'use strict';
const fs = require('node:fs');
const mib = Number(fs.readFileSync('size.txt', 'utf8').trim());
const chunk = Buffer.alloc(1 << 20, 0x61);
const fd = fs.openSync('fill.bin', 'w');
let written = 0;
try {
  while (written < mib) {
    fs.writeSync(fd, chunk);
    written += 1;
  }
} catch (e) {
  console.error(`ruum-fixture: fill stopped after ${written} MiB: ${e.code || e.message}`);
  process.exit(1);
} finally {
  fs.closeSync(fd);
}
console.error(`ruum-fixture: wrote ${written} MiB`);
