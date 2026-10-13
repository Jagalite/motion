// Controlled protocol adversary, isolated on loopback. No external dependencies.
import http from 'node:http';
import { generateKeyPairSync, sign } from 'node:crypto';
const keys = [0, 1].map(i => {
  const { privateKey, publicKey } = generateKeyPairSync('rsa', { modulusLength: 2048 });
  return { privateKey, jwk: { ...publicKey.export({ format: 'jwk' }), kid: `key-${i}`, alg: 'RS256', use: 'sig' } };
});
let active = 0;
const nonce = 'n'.repeat(64), verifier = 'v'.repeat(64);
const server = http.createServer(async (req, res) => {
  const origin = `http://127.0.0.1:${server.address().port}`;
  const reply = (status, value) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
  if (req.url === '/.well-known/openid-configuration') return reply(200, {
    issuer: origin, authorization_endpoint: `${origin}/authorize`, token_endpoint: `${origin}/token`, jwks_uri: `${origin}/jwks`,
    response_types_supported: ['code'], subject_types_supported: ['public'], id_token_signing_alg_values_supported: ['RS256'],
    token_endpoint_auth_methods_supported: ['client_secret_basic'], code_challenge_methods_supported: ['S256']
  });
  if (req.url === '/jwks') return reply(200, { keys: [keys[active].jwk] });
  if (req.url !== '/token' || req.method !== 'POST') return reply(404, {});
  let body = ''; for await (const chunk of req) { body += chunk; if (body.length > 16384) return reply(413, {}); }
  const form = new URLSearchParams(body), mode = form.get('code');
  if (form.get('code_verifier') !== verifier) return reply(400, { error: 'invalid_grant' });
  if (mode === 'rotate') active = 1;
  const now = Math.floor(Date.now() / 1000);
  const claims = { iss: origin, sub: 'verified-person', aud: 'motion-test', nonce, iat: now, auth_time: now, exp: now + 120 };
  if (mode === 'issuer') claims.iss = 'https://wrong.example.invalid';
  if (mode === 'audience') claims.aud = 'another-client';
  if (mode === 'nonce') claims.nonce = 'different-nonce';
  if (mode === 'expired') claims.exp = now - 60;
  if (mode === 'azp') { claims.aud = ['motion-test', 'another-client']; claims.azp = 'another-client'; }
  if (mode === 'old-auth') claims.auth_time = now - 600;
  const header = { alg: 'RS256', kid: mode === 'unknown-key' ? 'unknown' : keys[active].jwk.kid };
  const encode = value => Buffer.from(JSON.stringify(value)).toString('base64url');
  const unsigned = `${encode(header)}.${encode(claims)}`;
  const signature = sign('RSA-SHA256', Buffer.from(unsigned), keys[mode === 'signature' ? 1 - active : active].privateKey).toString('base64url');
  reply(200, { access_token: 'fixture-only', token_type: 'Bearer', expires_in: 120, id_token: `${unsigned}.${signature}` });
});
server.listen(0, '127.0.0.1', () => process.stdout.write(`${server.address().port}\n`));
