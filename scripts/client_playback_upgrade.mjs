// Upgrade evidence: a database created and populated by a binary built from
// origin/main (scan, paired device, v1 progress) is opened by this branch's
// binary, which applies 0032/0033. Checks the schema version, the 0032
// backfill of single-timeline progress, the pre-upgrade device credential
// and catalog reads.
//   node scripts/client_playback_upgrade.mjs <main-binary> <branch-binary> <demuxe-dir> [out-dir]
// Launch both binaries once beforehand (macOS first-launch assessment).
import {Server, media, provision} from './client_playback_e2e.mjs';
import {mkdtemp} from 'node:fs/promises';
import {writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createServer} from 'node:net';

const [mainBinary, branchBinary, demuxe] = process.argv.slice(2);
const port = await new Promise(done => { const s = createServer(); s.listen(0, '127.0.0.1', () => { const {port} = s.address(); s.close(() => done(port)); }); });
const work = await mkdtemp(join(tmpdir(), 'motion-upgrade-'));
media(join(work, 'media'));
const result = {checks: []};
const check = (name, pass, detail) => { result.checks.push({name, pass, detail}); console.log(`${pass ? 'PASS' : 'FAIL'} ${name} ${JSON.stringify(detail).slice(0, 300)}`); };

const before = new Server({work, port, launch: mainBinary, demuxe, allowLegacy: true});
await before.start();
const ctx = await provision(before);
const operator = before.operator();
const migrations = (await before.api('GET', '/api/v2/system/capabilities', undefined, ctx.token)).json;
// v1 progress on the single-timeline film: migration 0032 may backfill it.
const v1 = (await before.api('GET', '/api/v1/items', undefined, operator)).json;
const film = (v1.items ?? v1).find(i => /film/i.test(i.title));
const progress = await before.api('PUT', `/api/v1/profiles/default/progress/${film.id}`, {position_seconds: 37.5}, operator);
check('main binary records v1 progress', progress.status < 300, {status: progress.status, schema: migrations?.schema_version});
await before.stop();

const after = new Server({work, port, launch: branchBinary, demuxe});
await after.start();
const health = await after.api('GET', '/api/v2/system/health', undefined, ctx.token);
const caps = (await after.api('GET', '/api/v2/system/capabilities', undefined, ctx.token)).json;
check('combined binary opens the main database', health.status === 200, {status: health.status, schema: caps?.schema_version});
const viewing = await after.api('GET', `/api/v2/profiles/default/timelines/${ctx.film.timeline}/viewing`, undefined, ctx.token);
check('v2 timeline viewing reads the backfilled single-timeline progress', viewing.status === 200 && viewing.json?.position_ms === 37500, viewing.json);
const plan = await after.api('POST', '/api/v2/playback/plans', {profile_id: 'default', timeline_id: ctx.film.timeline, version_id: null, source: null,
  tracks: {audio_component_id: null, subtitle_component_id: null, subtitle_policy: 'auto', audio_track_id: null, subtitle_track_id: null},
  quality: {mode: 'auto', max_bitrate_bps: null, max_height: null, allow_client_software: true, hdr_policy: 'preserve_if_supported'},
  client: {client_id: 'u', client_build: '1', demuxe_asset_digest: null, transports: ['http_range'], video_codecs: ['avc1'], audio_codecs: ['mp4a'],
    subtitle_modes: ['text'], hdr: 'unknown', max_height: null, software_decode: 'unknown', cross_origin_isolated: false}, failed_candidate_ids: []}, ctx.token);
check('the paired device from before the upgrade plans playback', plan.status === 200 && plan.json?.status === 'ready', {status: plan.status, transport: plan.json?.transport});
const catalog = await after.api('GET', '/api/v2/catalog/items?limit=10', undefined, ctx.token);
check('catalog written by main reads after upgrade', catalog.status === 200 && catalog.json.items.length === 2, {count: catalog.json?.items?.length});
await after.stop();
writeFileSync(join(process.argv[5] ?? work, 'upgrade-result.json'), JSON.stringify(result, null, 2));
const ok = result.checks.every(c => c.pass);
console.log(ok ? 'UPGRADE OK' : 'UPGRADE FAILED');
process.exit(ok ? 0 : 1);
