/**
 * Open a session, preferring the one this workspace remembers.
 *
 * A stored id that no longer resolves is *refused* by the server rather than
 * quietly replaced, so the refusal is the cue to start over — **once**, with a
 * word said out loud about which id was dropped. Retrying a name the server
 * refuses is how a client turns a bad id into a loop.
 */
export async function withStoredSession<T>(
  stored: string | undefined,
  open: (id: string | undefined) => Promise<T>,
  onFallback: (id: string, error: unknown) => Promise<void> | void,
): Promise<T> {
  try {
    return await open(stored);
  } catch (error) {
    if (stored === undefined) throw error;
    await onFallback(stored, error);
    return open(undefined);
  }
}
