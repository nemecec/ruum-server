// Spike S-2: a small Node app. It echoes the request headers it received and checks the
// Ruum-User token as the SDK would (signature with the public key in RUUM_USER_PUBKEY, iss, aud,
// typ, env, numeric iat and exp, exp - iat <= 60 s). GET /cookies answers with Set-Cookie lines that the gateway must filter.
'use strict';
const http = require('node:http');
const crypto = require('node:crypto');

const PORT = Number(process.env.PORT || 8080);
const pub = process.env.RUUM_USER_PUBKEY ? crypto.createPublicKey(process.env.RUUM_USER_PUBKEY) : null;

function b64url(s) {
  return Buffer.from(s.replace(/-/g, '+').replace(/_/g, '/'), 'base64');
}

function checkToken(tok) {
  if (!pub) return { valid: false, reason: 'no key' };
  if (typeof tok !== 'string') return { valid: false, reason: 'missing' };
  const parts = tok.split('.');
  if (parts.length !== 3) return { valid: false, reason: 'shape' };
  let header, claims;
  try {
    header = JSON.parse(b64url(parts[0]).toString('utf8'));
    claims = JSON.parse(b64url(parts[1]).toString('utf8'));
  } catch {
    return { valid: false, reason: 'json' };
  }
  if (header.alg !== 'EdDSA') return { valid: false, reason: 'alg' };
  const ok = crypto.verify(null, Buffer.from(parts[0] + '.' + parts[1]), pub, b64url(parts[2]));
  if (!ok) return { valid: false, reason: 'signature' };
  const now = Math.floor(Date.now() / 1000);
  if (claims.iss !== process.env.RUUM_ISSUER) return { valid: false, reason: 'iss' };
  if (claims.aud !== process.env.RUUM_AUDIENCE) return { valid: false, reason: 'aud' };
  if (claims.typ !== 'app_user') return { valid: false, reason: 'typ' };
  if (claims.env !== process.env.RUUM_ENV) return { valid: false, reason: 'env' };
  // Both times must be numbers: without iat, exp - iat is NaN and a bound check would pass.
  if (!Number.isInteger(claims.iat) || !Number.isInteger(claims.exp)) return { valid: false, reason: 'exp' };
  if (claims.exp <= now || claims.iat > now + 5 || claims.exp - claims.iat > 60) {
    return { valid: false, reason: 'exp' };
  }
  return { valid: true, claims };
}

http
  .createServer((req, res) => {
    if (req.url === '/healthz') {
      res.end('ok\n');
      return;
    }
    if (req.url === '/cookies') {
      res.setHeader('Set-Cookie', [
        '__Host-ruum_app=fixation; Path=/; Secure; HttpOnly',
        '__Host-ruum_visit=fixation; Path=/; Secure',
        'ruum_session=fixation',
        'RUUM=x',
        '=ruum_app=nameless; Path=/',
        'ruum_app',
        'app_session=kept; Path=/; HttpOnly',
      ]);
      res.end('cookies\n');
      return;
    }
    const raw = [];
    for (let i = 0; i < req.rawHeaders.length; i += 2) raw.push([req.rawHeaders[i], req.rawHeaders[i + 1]]);
    const body = {
      raw_headers: raw,
      ruum_user: checkToken(req.headers['ruum-user']),
      ruum_user_count: raw.filter(([k]) => k.toLowerCase() === 'ruum-user').length,
    };
    res.setHeader('Content-Type', 'application/json');
    res.end(JSON.stringify(body));
  })
  .listen(PORT, '0.0.0.0', () => console.log(`app listening on ${PORT}`));
