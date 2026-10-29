import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
  const resource = useMemo(() => ({}), deps);
  const previousResource = useRef(resource);
  const generation = useRef({ active: false, inflight: false, again: false });

  const refresh = useCallback(async () => {
    const request = generation.current;
    if (!request.active) return;
    // A refresh asked for during a request (after a save, say) may need
    // data newer than that request saw: run once more when it ends.
    if (request.inflight) {
      request.again = true;
      return;
    }
    request.inflight = true;
    try {
      do {
        request.again = false;
        try {
          const v = await fnRef.current();
          if (!request.active) return;
          setData(v);
          setError(undefined);
        } catch (e) {
          if (request.active) setError(toCockpitError(e));
        }
      } while (request.again && request.active);
    } finally {
      request.inflight = false;
      if (request.active) setLoading(false);
    }
  }, []);

  useEffect(() => {
    const current = { active: true, inflight: false, again: false };
    generation.current = current;
    if (previousResource.current !== resource) {
      previousResource.current = resource;
      setData(undefined);
      setError(undefined);
    }
    setLoading(true);
    void refresh();
    if (!intervalMs) return () => void (current.active = false);
    const id = window.setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, intervalMs);
    const onVisible = () => document.visibilityState === "visible" && void refresh();
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      current.active = false;
      window.clearInterval(id);
      document.removeEventListener("visibilitychange", onVisible);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intervalMs, refresh, resource]);

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
