// Pure, pass-scoped coordination. Inventory is admitted only when complete;
// snapshots are observations of the owned cache, never authority to recreate a
// tombstoned demand. Each batch must finish before the next observation.
export function downloadReconciliation({operation, profile, scope, inventory, complete, snapshot, reconciled}) {
  if (!complete) throw new Error('Incomplete download inventory');
  assertOwnedSnapshot(snapshot, scope);
  const downloads = inventory.filter(d => d.profile_id === profile).map(d => structuredClone(d));
  const removed = [...new Set(snapshot.removals ?? [])];
  return {state: {operation, phase: 'removals', scope: {...scope}, downloads, removed, reconciled, transfers: []},
    effects: removed.map(id => ({type: 'DeleteDemand', id, requireRemovalIntent: true}))};
}
export function advanceDownloadReconciliation(state, input) {
  if (input.operation !== state.operation || state.phase === 'done') return {state, effects: []};
  if (input.snapshot) assertOwnedSnapshot(input.snapshot, state.scope);
  const next = {...state};
  let effects;
  const local = input.snapshot?.manifest?.downloads ?? [];
  const staged = input.snapshot?.staged ?? [];
  const saved = new Set(local.map(entry => entry.identity.download_id));
  const wanted = state.downloads.filter(d => !state.removed.includes(d.id));
  if (state.phase === 'removals' && input.type === 'RemovalsObserved') {
    next.phase = 'cleanup';
    effects = [...new Set([...saved, ...staged])].flatMap(id => {
      const listed = wanted.find(d => d.id === id);
      return listed?.state === 'ready' ? [] : [{type: 'RemoveLocal', id, listed, requireRemovalIntent: true}];
    });
  } else if (state.phase === 'cleanup' && input.type === 'CleanupObserved') {
    next.phase = 'reports';
    const pending = new Set(input.pendingReports);
    effects = state.downloads.flatMap(listed => {
      const reported = listed.device_transfer;
      const current = reported && reported.download_revision === listed.revision;
      if (listed.state === 'ready' && saved.has(listed.id) && !(current && reported.state === 'verified_local'))
        return [{type: 'Report', listed, status: 'verified_local', bytes: Number(listed.size_bytes) || 0}];
      if (listed.state !== 'ready' && !saved.has(listed.id) && !staged.includes(listed.id)
        && (pending.has(listed.id) || reported) && !(current && reported.state === 'removed'))
        return [{type: 'Report', listed, status: 'removed', bytes: 0}];
      return current && reported.state === 'removed' ? [{type: 'ForgetReport', id: listed.id}] : [];
    });
  } else if (state.phase === 'reports' && input.type === 'ReportsObserved') {
    next.phase = 'transfers';
    next.transfers = wanted.filter(d => d.state === 'ready' && !saved.has(d.id));
    effects = next.transfers.map(download => ({type: 'Transfer', download}));
  } else if (state.phase === 'transfers' && input.type === 'TransfersObserved') {
    // Collection cannot precede recovery of every eligible download. Require
    // an exact outcome for each identity, rather than an adapter success flag.
    const expected = state.transfers.map(d => d.id).sort();
    const actual = input.outcomes.map(result => result.id).sort();
    if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error('Incomplete transfer outcomes');
    if (input.outcomes.some(result => !['saved', 'failed'].includes(result.outcome))) throw new Error('Invalid transfer outcome');
    next.phase = 'collection';
    effects = input.outcomes.some(result => result.outcome === 'failed') ? [] : [{type: 'Collect'}];
  } else if (state.phase === 'collection' && input.type === 'CollectionObserved') {
    next.phase = 'done';
    effects = state.reconciled ? [{type: 'Rebase', manifest: input.snapshot.manifest}] : [];
  } else throw new Error(`Unexpected download reconciliation input ${input.type} in ${state.phase}`);
  return {state: next, effects};
}

function assertOwnedSnapshot(snapshot, scope) {
  if (!snapshot || !Array.isArray(snapshot.manifest?.downloads) || !Array.isArray(snapshot.staged)
    || !scope || ['server_id', 'principal_id', 'device_id', 'profile_id'].some(key =>
      typeof scope[key] !== 'string' || snapshot.manifest.scope?.[key] !== scope[key]))
    throw new Error('Cache observation does not match download owner');
}
