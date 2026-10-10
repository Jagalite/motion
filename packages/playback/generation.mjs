// Client ownership/lifetime coordinator. Server/core remain authoritative for
// staging, resource admission, generation activation and retirement.
export function createGenerationSwitcher({current, stage, prepare, activate, reconcile,
  commit, dispose, disruptive, fatal}) {
  let busy = false;
  return async function replace(active, change) {
    if (busy) throw new Error('A playback change is already in progress');
    if (!current()) return false;
    busy = true;
    let candidate = null;
    let activated = false;
    let cut = false;
    try {
      const staged = await stage(change, active.generation);
      if (!current()) return false;
      if (staged.id !== active.deliveryId || !staged.pending
        || staged.pending.generation === active.generation
        || !['overlap', 'disruptive'].includes(staged.replacement_mode)) {
        throw new Error('Invalid replacement delivery');
      }
      cut = staged.replacement_mode === 'disruptive';
      if (cut) await disruptive(active);
      if (!current()) return false;
      candidate = await prepare(staged);
      if (!current()) return false;
      let confirmed;
      try {
        confirmed = await activate(staged.id, staged.pending.generation, active.generation);
      } catch (error) {
        if (error.status && error.status < 500 && ![408, 425, 429].includes(error.status)) throw error;
        // An activation may have committed despite a lost response. Query the
        // server instead of assuming that the previous generation is usable.
        try { confirmed = await reconcile(staged.id); }
        catch { activated = true; throw new Error('The active generation could not be confirmed.'); }
      }
      if (confirmed.id !== staged.id) { activated = true; throw new Error('Replacement identity changed.'); }
      if (confirmed.active?.generation !== staged.pending.generation) {
        if (confirmed.active?.generation !== active.generation) activated = true;
        throw new Error('The replacement generation was not activated.');
      }
      activated = true;
      if (!current()) return false;
      await commit(candidate, confirmed);
      candidate = null; // The current player now owns this candidate.
      if (!cut) await dispose(active.element);
      return true;
    } catch (error) {
      if (current() && (activated || cut)) await fatal(error);
      throw error;
    } finally {
      if (candidate) await dispose(candidate.element);
      busy = false;
    }
  };
}
