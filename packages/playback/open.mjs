// Bounded candidate trials preserve every explicit plan input. Failed candidates
// inform the server planner; the client never widens source/timeline/track pins.
export async function openCandidates(input, {plan, admit, prepare, retire, dispose = async () => {}, current}) {
  const failed = [...(input.failed_candidate_ids ?? [])];
  const limit = input.quality.mode === 'auto' ? 3 : 1;
  let last;
  for (let attempt = 0; attempt < limit && current(); attempt++) {
    const choice = await plan({...input, failed_candidate_ids: [...failed]});
    if (!current()) return null;
    if (choice.status !== 'ready') throw new Error(`Playback is blocked: ${(choice.reason_codes ?? []).join(', ') || 'no compatible version'}`);
    if (choice.profile_id !== input.profile_id || choice.timeline_id !== input.timeline_id
      || typeof choice.candidate_id !== 'string' || failed.includes(choice.candidate_id)) {
      throw new Error('The planner returned a mismatched or already failed candidate.');
    }
    let delivery;
    try {
      delivery = await admit(choice);
      if (!current()) { await retire(delivery.id); return null; }
      const opened = await prepare(delivery);
      if (!current()) { await dispose(opened); await retire(delivery.id); return null; }
      return opened;
    } catch (error) {
      if (delivery) await retire(delivery.id);
      if (!current()) return null;
      if (error.status && error.status < 500) throw error;
      last = error;
      failed.push(choice.candidate_id);
    }
  }
  if (!current()) return null;
  throw last ?? new Error('No candidate could be opened.');
}
