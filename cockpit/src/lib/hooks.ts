import { useCallback, useEffect, useRef, useState } from "react";
import { CockpitError, toCockpitError } from "./transport";

export interface Polled<T> {
  data: T | undefined;
  error: CockpitError | undefined;
  loading: boolean;
  refresh: () => Promise<void>;
}

/**
 * Run `fn` now and every `intervalMs` while the window is visible.
 * Keeps the last good data when a refresh fails. `intervalMs = 0` = once.
 */
export function usePoll<T>(fn: () => Promise<T>, intervalMs: number, deps: unknown[] = []): Polled<T> {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<CockpitError>();
  const [loading, setLoading] = useState(true);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  const inflight = useRef(false);
  const alive = useRef(true);

  const refresh = useCallback(async () => {
    if (inflight.current) return;
    inflight.current = true;
    try {
      const v = await fnRef.current();
      if (!alive.current) return;
      setData(v);
      setError(undefined);
    } catch (e) {
      if (alive.current) setError(toCockpitError(e));
    } finally {
      inflight.current = false;
      if (alive.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    alive.current = true;
    setLoading(true);
    void refresh();
    if (!intervalMs) return () => void (alive.current = false);
    const id = window.setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, intervalMs);
    const onVisible = () => document.visibilityState === "visible" && void refresh();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      alive.current = false;
      window.clearInterval(id);
      document.removeEventListener("visibilitychange", onVisible);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intervalMs, refresh, ...deps]);

  return { data, error, loading, refresh };
}

/** Wrap an async action with a busy flag and error capture. */
export function useAction<A extends unknown[], R>(fn: (...args: A) => Promise<R>) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<CockpitError>();
  const run = useCallback(
    async (...args: A): Promise<R | undefined> => {
      setBusy(true);
      setError(undefined);
      try {
        return await fn(...args);
      } catch (e) {
        setError(toCockpitError(e));
        throw toCockpitError(e);
      } finally {
        setBusy(false);
      }
    },
    [fn],
  );
  return { run, busy, error };
}

export function sleep(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}
