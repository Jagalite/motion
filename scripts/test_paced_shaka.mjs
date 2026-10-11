// Exercises the patched upstream production classes, not a copied window model.
// Set MOTION_SHAKA_SOURCE to the verified, patched source used for compilation.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
import {runInNewContext} from 'node:vm';
const root = process.env.MOTION_SHAKA_SOURCE;
if (!root) throw new Error('MOTION_SHAKA_SOURCE must identify the patched upstream source');
function fixture(paced = true) {
  let now = 100000;
  const shaka = {hls: {}, media: {ManifestParser: {registerParserByMime() {}}}, log: {v1() {}}, util: {MimeUtils: {RAW_FORMATS: []}}};
  const context = {shaka, Date: {now: () => now}, goog: {provide() {}, require() {}, requireType() {}, DEBUG: false, asserts: {assert: assert.ok}}};
  for (const file of ['lib/util/array_utils.js', 'lib/media/presentation_timeline.js', 'lib/media/segment_index.js', 'lib/hls/hls_parser.js']) runInNewContext(readFileSync(join(root, file), 'utf8'), context);
  const timeline = new shaka.media.PresentationTimeline(0, 12);
  timeline.setStatic(false);
  timeline.notifySegments([{startTime: 0, endTime: 24}]);
  const parser = Object.create(shaka.hls.HlsParser.prototype);
  parser.presentationTimeline_ = timeline;
  parser.presentationType_ = 'LIVE';
  parser.config_ = {hls: {motionPaced: paced}, availabilityWindowOverride: NaN};
  parser.uriToStreamInfosMap_ = new Map();
  function streams(ranges) {
    parser.uriToStreamInfosMap_ = new Map(ranges.map(([type, start, end], i) => [String(i), {
      type, minTimestamp: start, maxTimestamp: end,
      stream: {type, segmentIndex: new shaka.media.SegmentIndex([{startTime: start, endTime: end}])},
    }]));
    parser.determineDuration_();
  }
  return {timeline, parser, streams, advance: seconds => { now += seconds * 1000; }};
}
test('paced bounds do not expire or grow during a prolonged encoder pause', () => {
  const f = fixture(); f.streams([['video', 0, 24], ['audio', 0.02, 24.02]]);
  f.advance(3600);
  assert.equal(f.timeline.getSegmentAvailabilityStart(), 0.02);
  assert.equal(f.timeline.getSafeSeekRangeStart(10), 0.02);
  assert.equal(f.timeline.getSegmentAvailabilityEnd(), 24);
  assert.equal(f.timeline.getSeekRangeEnd(), 24);
  assert.equal(f.timeline.isLive(), true);
});
test('rolling removal and publication update the intersection, excluding text', () => {
  const f = fixture(); f.streams([['video', 0, 24], ['audio', 0, 24]]);
  f.streams([['video', 12, 48], ['audio', 12.02, 47.98], ['text', 0, 100]]);
  assert.equal(f.timeline.getSeekRangeStart(), 12.02);
  assert.equal(f.timeline.getSeekRangeEnd(), 47.98);
  f.advance(600);
  assert.equal(f.timeline.getSeekRangeStart(), 12.02);
  assert.equal(f.timeline.getSeekRangeEnd(), 47.98);
});
test('disjoint track windows expose an empty interval, not their union', () => {
  const f = fixture(); f.streams([['video', 12, 24], ['audio', 30, 40]]);
  assert.equal(f.timeline.getSeekRangeStart(), f.timeline.getSeekRangeEnd());
});
test('unmodified live policy retains clock-driven availability', () => {
  const f = fixture(false); f.streams([['video', 0, 24]]);
  const before = f.timeline.getSegmentAvailabilityEnd(); f.advance(10);
  assert.equal(f.timeline.getSegmentAvailabilityEnd() - before, 10);
});

test('actual index eviction to empty clears cached timestamps and recovers', () => {
  const f = fixture(); f.streams([['video', 0, 24], ['audio', 0, 24]]);
  f.parser.uriToStreamInfosMap_.get('0').stream.segmentIndex.evict(24);
  f.parser.determineDuration_();
  assert.equal(f.timeline.getSeekRangeStart(), 0);
  assert.equal(f.timeline.getSeekRangeEnd(), 0);
  f.streams([['video', 24, 48], ['audio', 24, 48]]);
  assert.equal(f.timeline.getSeekRangeStart(), 24);
  assert.equal(f.timeline.getSeekRangeEnd(), 48);
  f.streams([]);
  assert.equal(f.timeline.getSeekRangeEnd(), 0);
});
test('zero endpoints are finite bounds', () => {
  const f = fixture(); f.streams([['video', 0, 0]]);
  assert.equal(f.timeline.getSeekRangeStart(), 0);
  assert.equal(f.timeline.getSeekRangeEnd(), 0);
});
test('completed presentation caps bounds at final duration', () => {
  const f = fixture(); f.streams([['video', 12, 48], ['audio', 12, 47.98]]);
  f.parser.updatePlaylistTimer_ = {stop() {}};
  f.parser.setPresentationType_('VOD');
  f.parser.determineDuration_();
  assert.equal(f.timeline.isLive(), false);
  assert.equal(f.timeline.getDuration(), 47.98);
  assert.equal(f.timeline.getSeekRangeStart(), 12);
  assert.equal(f.timeline.getSeekRangeEnd(), 47.98);
});
