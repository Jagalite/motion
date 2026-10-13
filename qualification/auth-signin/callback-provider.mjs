// Loopback-only OIDC adapter fault fixture. Holds token exchange until the test
// releases it, allowing observation of committed reservation and cancellation.
import http from 'node:http';
import {generateKeyPairSync, sign, createHash, randomUUID} from 'node:crypto';
const {privateKey, publicKey} = generateKeyPairSync('rsa', {modulusLength: 2048});
const jwk = {...publicKey.export({format: 'jwk'}), kid: 'fixture', alg: 'RS256', use: 'sig'};
const codes = new Map();
let exchanges = 0, release;
const gate = new Promise(resolve => { release = resolve; });
const server = http.createServer(async (req, res) => {
  const origin = `http://127.0.0.1:${server.address().port}`;
  const url = new URL(req.url, origin);
  const reply = (status, value) => { res.writeHead(status, {'content-type': 'application/json'}); res.end(JSON.stringify(value)); };
  if (url.pathname === '/.well-known/openid-configuration') return reply(200, {
    issuer: origin, authorization_endpoint: `${origin}/authorize`, token_endpoint: `${origin}/token`, jwks_uri: `${origin}/jwks`,
    response_types_supported: ['code'], subject_types_supported: ['public'], id_token_signing_alg_values_supported: ['RS256'],
    token_endpoint_auth_methods_supported: ['client_secret_basic'], code_challenge_methods_supported: ['S256']
  });
  if (url.pathname === '/jwks') return reply(200, {keys: [jwk]});
  if (url.pathname === '/fixture/status') return reply(200, {exchanges});
  if (url.pathname === '/fixture/release' && req.method === 'POST') { release(); return reply(200, {}); }
  if (url.pathname === '/authorize') {
    const code = randomUUID(); codes.set(code, url.searchParams);
    const redirect = new URL(url.searchParams.get('redirect_uri'));
    redirect.searchParams.set('code', code); redirect.searchParams.set('state', url.searchParams.get('state'));
    res.writeHead(302, {location: redirect.href}); return res.end();
  }
  if (url.pathname !== '/token' || req.method !== 'POST') return reply(404, {});
  let body = ''; for await (const chunk of req) { body += chunk; if (body.length > 16384) return reply(413, {}); }
  exchanges++;
  await gate;
  const form = new URLSearchParams(body), saved = codes.get(form.get('code'));
  if (!saved || createHash('sha256').update(form.get('code_verifier') ?? '').digest('base64url') !== saved.get('code_challenge')) return reply(400, {error: 'invalid_grant'});
  codes.delete(form.get('code'));
  const now = Math.floor(Date.now() / 1000);
  const encode = value => Buffer.from(JSON.stringify(value)).toString('base64url');
  const claims = {iss: origin, sub: 'adapter-person', aud: saved.get('client_id'), nonce: saved.get('nonce'), iat: now, auth_time: now, exp: now + 120};
  const unsigned = `${encode({alg: 'RS256', kid: 'fixture'})}.${encode(claims)}`;
  const signature = sign('RSA-SHA256', Buffer.from(unsigned), privateKey).toString('base64url');
  reply(200, {access_token: 'fixture-only', token_type: 'Bearer', expires_in: 120, id_token: `${unsigned}.${signature}`});
});
server.listen(0, '127.0.0.1', () => process.stdout.write(`${server.address().port}\n`));
