// The hello app of the builder's tests (card R-2b): answers every request on $PORT.
'use strict';
const http = require('node:http');
http
  .createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('hello from ruum\n');
  })
  .listen(Number(process.env.PORT || 8080), '0.0.0.0');
