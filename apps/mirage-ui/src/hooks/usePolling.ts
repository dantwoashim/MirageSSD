import { useEffect, useRef } from 'react';

/**
 * Runs `callback` immediately, awaits it, then schedules the next run after
 * `intervalMs`. Pauses while the window is hidden and runs again as soon as
 * it becomes visible. The latest callback is always used; the timer chain is
 * cleaned up on unmount or when disabled.
 */
export function usePolling(
  callback: () => void | Promise<void>,
  { intervalMs, enabled = true, pauseWhenHidden = true }: { intervalMs: number; enabled?: boolean; pauseWhenHidden?: boolean },
) {
  const latest = useRef(callback);
  useEffect(() => {
    latest.current = callback;
  });

  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let inFlight = false;
    let timer: number | undefined;

    const run = async () => {
      if (stopped || inFlight) return;
      inFlight = true;
      try {
        await latest.current();
      } finally {
        inFlight = false;
      }
      if (!stopped) timer = window.setTimeout(() => void run(), intervalMs);
    };

    if (pauseWhenHidden && typeof document !== 'undefined' && document.hidden) {
      // Start paused; the visibility listener kicks off the first run.
    } else {
      void run();
    }

    const onVisibility = () => {
      if (document.hidden) {
        window.clearTimeout(timer);
      } else {
        void run();
      }
    };
    if (pauseWhenHidden && typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', onVisibility);
    }
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      if (pauseWhenHidden && typeof document !== 'undefined') {
        document.removeEventListener('visibilitychange', onVisibility);
      }
    };
  }, [intervalMs, enabled, pauseWhenHidden]);
}
