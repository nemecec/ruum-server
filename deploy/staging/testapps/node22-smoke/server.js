// The node22 smoke app (card I6-13): answers the health check and one page with a fixed text,
// which ops/smoke-apps.sh looks for (STG_SMOKE_NODE22_BODY=ruum-node22-smoke). It prints no
// value of its environment.
const http = require('http');
const fs = require('fs');
const built = fs.existsSync('built.txt') ? 'yes' : 'no';
http
  .createServer((req, res) => {
    if (req.url === '/healthz') {
      res.writeHead(200, { 'content-type': 'text/plain' });
      res.end('ok\n');
      return;
    }
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end(`ruum-node22-smoke (postinstall: ${built})\n`);
  })
  .listen(Number(process.env.PORT || 8080));
