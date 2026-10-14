// Independent DKIM verifier for Motion's adapter tests (RFC 6376, rsa-sha256,
// relaxed/relaxed). Written separately from the Rust signer and uses Node's
// crypto for RSA verification, so a passing check is not self-referential.
//
// node dkim_verify.mjs <message.eml> <published TXT record>
// Prints JSON {"ok":bool,"reason":string,"d":..,"s":..}.
import {readFileSync} from 'node:fs';
import {createHash, createPublicKey, verify} from 'node:crypto';

function fail(reason) {
  process.stdout.write(JSON.stringify({ok: false, reason}) + '\n');
  process.exit(0);
}

const [file, record] = process.argv.slice(2);
const raw = readFileSync(file).toString('latin1');
const split = raw.indexOf('\r\n\r\n');
if (split < 0) fail('no header/body separator');
const head = raw.slice(0, split + 2);
const body = raw.slice(split + 4);

// Header fields in order, each with its exact raw text including folding.
const fields = [];
for (const line of head.split('\r\n').slice(0, -1)) {
  if (/^[ \t]/.test(line)) {
    if (!fields.length) fail('continuation before first header');
    fields[fields.length - 1].raw += '\r\n' + line;
  } else {
    const colon = line.indexOf(':');
    if (colon < 1) fail('malformed header');
    fields.push({name: line.slice(0, colon), raw: line});
  }
}

const relaxedHeader = field => {
  const colon = field.raw.indexOf(':');
  const name = field.raw.slice(0, colon).trim().toLowerCase();
  const value = field.raw.slice(colon + 1).replace(/\r\n/g, '').replace(/[ \t]+/g, ' ').trim();
  return `${name}:${value}`;
};
const relaxedBody = text => {
  let lines = text.split('\r\n').map(line => line.replace(/[ \t]+/g, ' ').replace(/ $/, ''));
  while (lines.length && lines[lines.length - 1] === '') lines.pop();
  return lines.map(line => line + '\r\n').join('');
};

const signature = fields.find(f => f.name.toLowerCase() === 'dkim-signature');
if (!signature) fail('no DKIM-Signature');
const value = signature.raw.slice(signature.raw.indexOf(':') + 1);
const tags = new Map();
for (const part of value.split(';')) {
  const eq = part.indexOf('=');
  if (eq < 0) { if (part.trim()) fail('malformed tag'); continue; }
  const key = part.slice(0, eq).replace(/[\r\n \t]/g, '');
  if (tags.has(key)) fail(`duplicate tag ${key}`);
  tags.set(key, part.slice(eq + 1).replace(/[\r\n \t]/g, ''));
}
for (const required of ['v', 'a', 'b', 'bh', 'd', 'h', 's']) if (!tags.has(required)) fail(`missing ${required}=`);
if (tags.get('v') !== '1') fail('unsupported version');
if (tags.get('a') !== 'rsa-sha256') fail('unsupported algorithm');
if (tags.get('c') !== 'relaxed/relaxed') fail('unsupported canonicalization');
if (!tags.get('h').split(':').map(h => h.toLowerCase()).includes('from')) fail('From is not signed');

const bodyHash = createHash('sha256').update(relaxedBody(body), 'latin1').digest('base64');
if (bodyHash !== tags.get('bh')) fail('body hash mismatch');

// Select header instances bottom-up, each used at most once (RFC 6376 §5.4.2).
const used = new Set();
let data = '';
for (const name of tags.get('h').split(':').map(h => h.trim().toLowerCase())) {
  for (let i = fields.length - 1; i >= 0; i--) {
    if (used.has(i) || fields[i] === signature || fields[i].name.trim().toLowerCase() !== name) continue;
    used.add(i);
    data += relaxedHeader(fields[i]) + '\r\n';
    break;
  }
}
// The signature field itself with an empty b= value and no trailing CRLF.
const stripped = signature.raw.replace(/(^|;)([ \t\r\n]*b[ \t\r\n]*=)[^;]*/, '$1$2');
data += relaxedHeader({raw: stripped});

const published = new Map(record.split(';').map(p => p.trim()).filter(Boolean).map(p => {
  const eq = p.indexOf('=');
  return [p.slice(0, eq).trim(), p.slice(eq + 1).replace(/\s/g, '')];
}));
if ((published.get('k') ?? 'rsa') !== 'rsa' || !published.get('p')) fail('unusable key record');
const key = createPublicKey({key: Buffer.from(published.get('p'), 'base64'), format: 'der', type: 'spki'});
const ok = verify('sha256', Buffer.from(data, 'latin1'), key, Buffer.from(tags.get('b'), 'base64'));
process.stdout.write(JSON.stringify({ok, reason: ok ? 'verified' : 'signature mismatch', d: tags.get('d'), s: tags.get('s')}) + '\n');
