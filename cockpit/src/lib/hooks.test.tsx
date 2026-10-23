import { act, renderHook, waitFor } from "@testing-library/react";
import { usePoll } from "./hooks";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe("usePoll request ownership", () => {
  it("fetches new dependencies immediately and ignores the old response", async () => {
    const old = deferred<string>();
    const next = deferred<string>();
    const fetches: string[] = [];
    const { result, rerender } = renderHook(({ key }) => usePoll(() => {
      fetches.push(key);
      return key === "old" ? old.promise : next.promise;
    }, 0, [key]), { initialProps: { key: "old" } });
    rerender({ key: "next" });
    expect(fetches).toEqual(["old", "next"]);
    await act(async () => next.resolve("current"));
    await act(async () => old.resolve("stale"));
    expect(result.current.data).toBe("current");
    expect(result.current.loading).toBe(false);
  });

  it("clears previous data when the requested resource changes", async () => {
    const pending = deferred<string>();
    const { result, rerender } = renderHook(({ key }) => usePoll(
      () => key === "old" ? Promise.resolve("old secrets") : pending.promise, 0, [key],
    ), { initialProps: { key: "old" } });
    await waitFor(() => expect(result.current.data).toBe("old secrets"));
    rerender({ key: "next" });
    expect(result.current.data).toBeUndefined();
    expect(result.current.loading).toBe(true);
    await act(async () => pending.resolve("new secrets"));
    expect(result.current.data).toBe("new secrets");
  });

  it("ignores rejected requests from a previous dependency generation", async () => {
    const old = deferred<string>();
    const { result, rerender } = renderHook(({ key }) => usePoll(
      () => key === "old" ? old.promise : Promise.resolve("current"), 0, [key],
    ), { initialProps: { key: "old" } });
    rerender({ key: "next" });
    await act(async () => old.reject(new Error("old server offline")));
    expect(result.current.error).toBeUndefined();
    expect(result.current.data).toBe("current");
  });

  it("survives StrictMode effect cleanup with a request in flight", async () => {
    const pending: ReturnType<typeof deferred<string>>[] = [];
    const { result } = renderHook(() => usePoll(() => {
      const request = deferred<string>();
      pending.push(request);
      return request.promise;
    }, 0), { reactStrictMode: true });
    expect(pending).toHaveLength(2);
    await act(async () => pending.at(-1)!.resolve("live"));
    expect(result.current.data).toBe("live");
    expect(result.current.loading).toBe(false);
    await act(async () => pending[0]!.resolve("discarded"));
    expect(result.current.data).toBe("live");
  });

  it("keeps the last good data when a refresh of the same resource fails", async () => {
    let fail = false;
    const { result } = renderHook(() => usePoll(() => fail ? Promise.reject(new Error("offline")) : Promise.resolve("cached"), 0));
    await waitFor(() => expect(result.current.data).toBe("cached"));
    fail = true;
    await act(async () => result.current.refresh());
    expect(result.current.data).toBe("cached");
    expect(result.current.error?.message).toBe("offline");
  });
});
