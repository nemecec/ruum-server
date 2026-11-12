// Spike S-2 point 5: a hostile postinstall. It runs the probe (TCP connects to the metadata
// address, the platform Postgres and the other targets, the metadata GET, DNS, the Docker socket,
// the environment) from inside the build sandbox and writes what it reached to /out.
'use strict';
process.env.PROBE_OUT = process.env.PROBE_OUT || '/out/postinstall.json';
require(process.env.PROBE_JS || '/probe/probe.js');
