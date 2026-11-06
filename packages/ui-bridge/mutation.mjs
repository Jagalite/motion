// Transport uncertainty is client state. A refusal before receipt lookup does
// not settle an earlier unanswered attempt. This matches terminal Intent.settle.
export function mutationOutcome(unknown, error) {
  if (!error) return 'settled';
  const status = error.status;
  const receiptConflict = ['idempotency_key_reused', 'idempotent_replay_unavailable'].includes(error.code);
  if (receiptConflict) return 'unknown';
  if (unknown) return [409, 412].includes(status) ? 'settled' : 'unknown';
  return status >= 400 && status < 500 && ![408, 425, 429].includes(status) ? 'settled' : 'unknown';
}

// Scope is non-secret identity, never a credential or a CSRF token. Server
// history and principal changes cannot silently reuse another owner's intent.
export function mutationScope(document, location) {
  const meta = name => document.querySelector(`meta[name="${name}"]`)?.content ?? '';
  return JSON.stringify([location.origin ?? '', meta('motion-server-epoch'), meta('motion-principal-id')]);
}
