// Native adapter ownership: no catalog/viewing authority is decided here.
// Every close event is blocked until this exact preparation has succeeded.
export function closeGate({prepare, finish, failed}) {
  let phase = 'open';
  let pending;
  return event => {
    if (phase === 'ready') return;
    event.preventDefault();
    if (phase === 'preparing') return pending;
    phase = 'preparing';
    pending = Promise.resolve().then(prepare).then(() => {
      phase = 'ready';
      finish();
    }).catch(error => {
      phase = 'open';
      failed(error);
    });
    return pending;
  };
}

// A late startup result still belongs to this host and must be retired when
// its intended connection has been superseded, even before attachment begins.
export async function attachOwnedServer(server, current, attach) {
  const local = await server.start();
  try {
    if (!current()) throw new Error('Local connection cancelled');
    return await attach(local);
  } catch (error) {
    await server.stop();
    throw error;
  }
}
