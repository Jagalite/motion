import {spawn} from 'node:child_process';
import {mkdtempSync, mkdirSync, readdirSync, readFileSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import electron from 'electron';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const staged = mkdtempSync(join(tmpdir(), 'motion-shell-sources-'));
const hashes = {};
for (const relative of [
  ...readdirSync(join(root, 'apps/desktop/src')).map(name => `apps/desktop/src/${name}`),
  'apps/desktop/test/shell-smoke.mjs', 'contracts/Motion_Server_API_v2.yaml',
]) {
  const bytes = readFileSync(join(root, relative));
  const destination = join(staged, relative);
  mkdirSync(join(destination, '..'), {recursive: true});
  writeFileSync(destination, bytes, {flag: 'wx'});
  hashes[relative] = createHash('sha256').update(bytes).digest('hex');
}
const output = join(staged, 'receipt.json');
const env = {...process.env, MOTION_SHELL_RECEIPT: output};
delete env.ELECTRON_RUN_AS_NODE;
const child = spawn(process.env.MOTION_ELECTRON_BINARY || electron, [join(staged, 'apps/desktop/test/shell-smoke.mjs')], {env, stdio: 'inherit'});
const timer = setTimeout(() => child.kill('SIGKILL'), 180000);
const code = await new Promise((resolve, reject) => {
  child.once('error', reject);
  child.once('exit', resolve);
}).finally(() => clearTimeout(timer));
let receipt;
try { receipt = JSON.parse(readFileSync(output, 'utf8')); }
catch { receipt = {passed: false, checks: {}, failure: 'Electron exited or timed out before writing its receipt', exit_code: code}; }
receipt.source_sha256 = hashes;
receipt.source_location = 'byte-identical temporary copy';
const directory = join(root, 'qualification/desktop');
mkdirSync(directory, {recursive: true});
const file = join(directory, `native-shell-${process.platform}-${process.arch}.json`);
writeFileSync(file, `${JSON.stringify(receipt, null, 2)}\n`);
console.log(`Native shell receipt: ${file}`);
process.exit(code === 0 && receipt.passed ? 0 : 1);
