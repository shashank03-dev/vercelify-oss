import { useEffect, useState } from "react";
import { compareVersions } from "./caps";
import { call } from "./transport";

/** The newest published release (what `vercelify update` and install-mac.sh install). */
export interface LatestRelease {
  version: string;
  /** Its GitHub release page, with the .dmg. */
  url: string;
  /** This app's version. */
  current: string;
}

// One request per few hours for the whole app (GitHub allows 60 an hour unsigned).
const TTL = 6 * 3600 * 1000;
let cached: { at: number; value: Promise<LatestRelease | undefined> } | undefined;

export function latestRelease(now = Date.now()): Promise<LatestRelease | undefined> {
  if (!cached || now - cached.at > TTL) {
    // Offline, rate limited or browser mode: no update notice, nothing else.
    cached = { at: now, value: call<LatestRelease>("latest_release").catch(() => undefined) };
  }
  return cached.value;
}

/** For tests. */
export function forgetLatestRelease() {
  cached = undefined;
}

export function useLatestRelease(): LatestRelease | undefined {
  const [release, setRelease] = useState<LatestRelease>();
  useEffect(() => {
    let live = true;
    const load = () => void latestRelease().then((r) => live && setRelease(r));
    load();
    const id = window.setInterval(load, TTL);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, []);
  return release;
}

/** Whether this app is older than the newest release. */
export function appBehind(r: LatestRelease | undefined): boolean {
  return !!r && compareVersions(r.current, r.version) < 0;
}
