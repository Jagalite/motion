// Cross-delivery ownership decisions, independent of DOM, HTTP, players and
// clocks. Operation identity fences late observations from a retired page.
export function deliveryReplacement(operation, predecessor, requiresBinding) {
  return {operation, predecessor, requiresBinding, replacement: null, prepared: false,
    binding: 'old', leaseFailed: false, retired: false, swapped: false, finished: false};
}
export function advanceDeliveryReplacement(state, input) {
  if (input.operation !== state.operation || state.finished) return {state, effects: []};
  const next = {...state};
  let effects = [];
  switch (input.type) {
    case 'Admitted':
      if (state.replacement === null && typeof input.delivery === 'string' && input.delivery !== state.predecessor) next.replacement = input.delivery;
      break;
    case 'Prepared':
      if (input.delivery === state.replacement) next.prepared = true;
      break;
    case 'LeaseFailed': next.leaseFailed = true; break;
    case 'PageRetired': next.retired = true; break;
    case 'RebindUnknown': if (state.binding === 'old') next.binding = 'unknown'; break;
    case 'RebindAcknowledged':
    case 'ReadBack':
      // The old binding does not disprove a PUT still in flight. Only an
      // exact replacement observation establishes the new binding.
      if (input.delivery === state.replacement && state.replacement !== null) next.binding = 'new';
      break;
    case 'Swap':
      if (!state.retired && !state.leaseFailed && state.prepared
        && (!state.requiresBinding || state.binding === 'new')) {
        next.swapped = true;
        next.finished = true;
        effects = [{type: 'Install', delivery: state.replacement}, {type: 'Retire', delivery: state.predecessor}];
      }
      break;
    case 'Failed':
      next.finished = true;
      if (state.replacement !== null) effects.push({type: 'Retire', delivery: state.replacement});
      if (!state.retired) effects.push({type: state.binding === 'old' ? 'Resume' : 'Stop', delivery: state.predecessor});
      break;
    default: throw new Error('Unknown delivery replacement input');
  }
  return {state: next, effects};
}
