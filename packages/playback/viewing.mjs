// Browser transport state, not viewing authority. The server/core validates
// authority, manual epochs, consecutive sequences and exact duplicates.
export function createViewingWriter(session, {send, persist, uuid, wait,
  pending = null, latest = null, current = () => true, changed = () => {}, archive = () => {}}) {
  let accepted = {...session};
  let inFlight = null;
  let rejected = false;
  let sealed = latest?.status === 'stopped' || pending?.status === 'stopped';
  const revision = value => typeof value === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(value) && BigInt(value) <= 18446744073709551615n;
  if (!revision(session.sequence)) throw new Error('Invalid viewing sequence');
  const save = () => persist(pending || latest ? {session: accepted, pending, latest} : null);
  const snapshot = value => {
    if (!Number.isSafeInteger(value.position_ms) || value.position_ms < 0
      || !['playing', 'paused', 'ended', 'stopped'].includes(value.status)
      || !revision(value.delivery_generation)) throw new Error('Invalid viewing observation');
    return {position_ms: value.position_ms, status: value.status, delivery_generation: value.delivery_generation};
  };
  if (pending) {
    snapshot(pending);
    if (!revision(pending.sequence) || BigInt(pending.sequence) !== BigInt(session.sequence) + 1n
      || typeof pending.event_id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(pending.event_id)) {
      throw new Error('Invalid saved viewing event');
    }
    pending = Object.freeze({...pending});
  }
  if (latest) latest = snapshot(latest);
  function record(value) {
    if (rejected || sealed || !current()) return;
    latest = snapshot(value);
    sealed = latest.status === 'stopped';
    save(); // Persist before sending; no credentials or CSRF tokens are stored.
  }
  async function drain() {
    let attempts = 0;
    while ((pending || latest) && !rejected && current()) {
      if (!pending) {
        const sequence = (BigInt(accepted.sequence) + 1n).toString();
        if (!revision(sequence)) throw new Error('Viewing sequence exhausted');
        pending = Object.freeze({...latest, event_id: uuid(), sequence});
        latest = null;
      }
      // A previous persistence failure must not let a later flush bypass durability.
      save();
      try {
        const ack = await send(accepted.id, pending);
        if (!current()) return false;
        const next = ack?.session;
        if (!next || next.id !== accepted.id || next.delivery_id !== accepted.delivery_id
          || next.profile_id !== accepted.profile_id || next.timeline_id !== accepted.timeline_id
          || next.sequence !== pending.sequence || next.position_ms !== pending.position_ms
          || next.status !== pending.status) throw new Error('Invalid viewing acknowledgement');
        accepted = {...next};
        pending = null;
        attempts = 0;
        save();
        changed(ack);
      } catch (error) {
        if (!current()) return false;
        if (error.status && error.status < 500 && ![408, 425, 429].includes(error.status)) {
          // Rejected authority cannot be retried as a new canonical event.
          // Retain the last rejected record separately for explicit reconciliation.
          archive({session: accepted, pending, latest, status: error.status});
          rejected = true;
          pending = latest = null;
          save();
          throw error;
        }
        if (++attempts >= 3) return false;
        await wait(250 * attempts);
      }
    }
    return !pending && !latest;
  }
  function flush() {
    if (!inFlight) inFlight = drain().finally(() => { inFlight = null; });
    return inFlight;
  }
  return {record, flush, get session() { return {...accepted}; }, get rejected() { return rejected; }};
}

// A viewing-session start names the revision the page rendered. A newer
// revision may be this browser's own previous player delivering its final
// event during unload, after this page rendered. Adopt it only when the
// current authority is still that session of ours (another device's start
// replaces session_id), no manual change happened (same manual epoch, no
// override, unwatched) and the resume position is within tolerance. Anything
// else fails closed and must be re-read.
export function adoptableRevision(current, rendered, toleranceMs = 2000) {
  const revision = typeof current?.revision === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(current.revision);
  if (!revision || current.watched !== false || current.manual_watched !== null) return null;
  if (typeof rendered?.ownSession !== 'string' || current.session_id !== rendered.ownSession) return null;
  if (current.manual_epoch !== rendered.manualEpoch) return null;
  const renderedResumeMs = rendered.resumeMs;
  if (!Number.isSafeInteger(current.position_ms) || !Number.isSafeInteger(renderedResumeMs)) return null;
  return Math.abs(current.position_ms - renderedResumeMs) <= toleranceMs ? current.revision : null;
}
