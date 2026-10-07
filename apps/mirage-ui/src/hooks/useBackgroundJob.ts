import { useCallback, useEffect, useRef, useState } from 'react';

/**
 * Drives the host's "start, then poll a status endpoint until it settles"
 * pattern: `start` runs the kickoff call and marks the job in flight; polling
 * continues until `in_flight` clears, then `onSettled` gets the final status.
 * Poll failures settle into `{ in_flight: false, error }`. Cancel-safe on
 * unmount.
 */
export function useBackgroundJob<S extends { in_flight?: boolean; step?: string; error?: string }>({
  fetchStatus,
  intervalMs,
  onSettled,
}: {
  fetchStatus: () => Promise<S>;
  intervalMs: number;
  onSettled?: (status: S) => void;
}) {
  const [status, setStatus] = useState<S | undefined>();
  const latest = useRef({ fetchStatus, onSettled });
  useEffect(() => {
    latest.current = { fetchStatus, onSettled };
  });

  useEffect(() => {
    if (!status?.in_flight) return;
    let stopped = false;
    let timer: number | undefined;
    const tick = async () => {
      if (stopped) return;
      try {
        const next = await latest.current.fetchStatus();
        if (stopped) return;
        setStatus(next);
        if (next.in_flight) {
          timer = window.setTimeout(() => void tick(), intervalMs);
          return;
        }
        latest.current.onSettled?.(next);
      } catch (failure) {
        if (stopped) return;
        const failed = {
          in_flight: false,
          error: failure instanceof Error ? failure.message : String(failure),
        } as unknown as S;
        setStatus(failed);
        latest.current.onSettled?.(failed);
      }
    };
    timer = window.setTimeout(() => void tick(), intervalMs);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, [status?.in_flight, intervalMs]);

  const start = useCallback(async (startFn: () => Promise<unknown>) => {
    await startFn();
    setStatus({ in_flight: true, step: 'starting' } as unknown as S);
  }, []);

  return { status, running: Boolean(status?.in_flight), start };
}
