import { invoke } from "@tauri-apps/api/core";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { AgentEvent, AlertsStatus, CoolifyApplication, Health, Job, SystemSnapshot } from "../types";
import { agent, coolify, isWorker } from "./api";
import { hasCap } from "./caps";
import { usePoll, type Polled } from "./hooks";
import { alertText, notify, shouldNotify } from "./notify";
import { useSession } from "./session";
import { inTauri } from "./transport";
import type { Alert } from "../types";

type Listener = () => void;

interface Live {
  /** The selected server's system snapshot, every 10 s. */
  system: Polled<SystemSnapshot>;
  /** Its Coolify applications every 15 s (always empty on a worker). */
  apps: Polled<CoolifyApplication[]>;
  /** Version and capabilities of its agent, every minute. */
  health: Polled<Health>;
  /** Alerts the server raised (power, disks, deploys…). */
  alerts: Polled<AlertsStatus | null>;
  /** Running and recent jobs (backups, moves, downloads, deploys), newest first. */
  jobs: Job[];
  /** Connected to the server's live event stream (else everything polls). */
  streaming: boolean;
  /** Apps stopped because nobody visited them (they wake on the next visit). */
  asleep: Set<string>;
  /** Call `fn` when the server says `topic` changed (or a job of that kind moved). */
  subscribe: (topic: string, fn: Listener) => () => void;
}

const Ctx = createContext<Live | null>(null);

/** Merge a job update into the list (newest first, bounded). */
export function mergeJob(list: Job[], job: Job): Job[] {
  const i = list.findIndex((j) => j.id === job.id);
  if (i >= 0) return list.map((j, k) => (k === i ? job : j));
  return [job, ...list].slice(0, 60);
}

/** Open problems: the newest alert per source, while it isn't resolved. */
export function openProblems(alerts: Alert[]): number {
  const seen = new Set<string>();
  let n = 0;
  for (const a of alerts) {
    if (seen.has(a.source)) continue;
    seen.add(a.source);
    if (!a.resolved && (a.status === "bad" || a.status === "warn")) n++;
  }
  return n;
}

/** The menu-bar item's line. */
export function trayText(hostname: string, reachable: boolean, problems: number): string {
  if (!reachable) return `${hostname} · not reachable`;
  return `${hostname} · ${problems ? `${problems} need${problems === 1 ? "s" : ""} attention` : "all good"}`;
}

/** Seconds to wait before reconnecting after `failures` failed attempts. */
export function backoff(failures: number): number {
  return Math.min(60, 2 ** Math.min(failures, 6));
}

/**
 * One shared connection for the chrome (sidebar, top bar, notifications) so
 * each piece doesn't fetch the same data on its own. With a current agent
 * that is a live event stream: alerts and job progress arrive as they
 * happen and pages refresh when the server says something changed. Older
 * agents are polled as before.
 */
export function LiveProvider({ children }: { children: ReactNode }) {
  const { settings } = useSession();
  const main = !isWorker(settings);
  const system = usePoll(() => agent.system(), 10_000, [settings.agent_url]);
  const apps = usePoll(() => (main ? coolify.applications() : Promise.resolve([])), main ? 15_000 : 0, [settings.agent_url, main]);
  const health = usePoll(() => agent.health(), 60_000, [settings.agent_url]);
  const withAlerts = hasCap(health.data, "alerts");
  const withEvents = hasCap(health.data, "events");
  const withJobs = hasCap(health.data, "jobs");
  const [streaming, setStreaming] = useState(false);
  // While streaming, alerts arrive as events; a slow poll only catches up.
  const alerts = usePoll(
    () => (withAlerts ? agent.alerts() : Promise.resolve(null)),
    withAlerts ? (streaming ? 300_000 : 30_000) : 0,
    [settings.agent_url, withAlerts],
  );
  const [jobs, setJobs] = useState<Job[]>([]);
  const listeners = useRef(new Map<string, Set<Listener>>());

  const emit = useCallback((topic: string) => {
    const fire = (t: string) => listeners.current.get(t)?.forEach((fn) => fn());
    if (topic === "all") listeners.current.forEach((set) => set.forEach((fn) => fn()));
    else fire(topic);
  }, []);

  const subscribe = useCallback((topic: string, fn: Listener) => {
    const set = listeners.current.get(topic) ?? new Set();
    set.add(fn);
    listeners.current.set(topic, set);
    return () => void set.delete(fn);
  }, []);

  // Jobs: loaded once per server, then kept current by events (or polled).
  useEffect(() => {
    setJobs([]);
    if (!withJobs) return;
    let alive = true;
    const load = () =>
      agent
        .jobs()
        .then((j) => alive && setJobs(j))
        .catch(() => {});
    void load();
    const id = streaming ? 0 : window.setInterval(load, 20_000);
    return () => {
      alive = false;
      if (id) window.clearInterval(id);
    };
  }, [settings.agent_url, withJobs, streaming]);

  const refreshAlerts = alerts.refresh;
  useEffect(() => {
    if (!withEvents) return;
    let stopped = false;
    let failures = 0;
    let cancel: () => void = () => {};
    let timer = 0;
    const host = settings.hostname;
    const connect = () => {
      const s = agent.events((e: AgentEvent) => {
        if (e.kind === "hello") {
          failures = 0;
          setStreaming(true);
          // Anything may have changed while we were away.
          emit("all");
          void refreshAlerts();
        } else if (e.kind === "alert" && e.alert) {
          void refreshAlerts();
          emit("alerts");
          if (shouldNotify(e.alert)) {
            const { title, body } = alertText(host, e.alert);
            void notify(title, body);
          }
        } else if (e.kind === "job" && e.job) {
          const job = e.job;
          setJobs((list) => mergeJob(list, job));
          if (job.status !== "running") emit(job.kind);
        } else if (e.kind === "changed" && e.topic) {
          emit(e.topic);
        }
      });
      cancel = s.cancel;
      s.done
        .catch(() => {})
        .finally(() => {
          setStreaming(false);
          if (stopped) return;
          failures += 1;
          timer = window.setTimeout(connect, backoff(failures) * 1000);
        });
    };
    connect();
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      cancel();
      setStreaming(false);
    };
  }, [settings.agent_url, settings.hostname, withEvents, emit, refreshAlerts]);

  const withSleep = main && hasCap(health.data, "sleep");
  const sleep = usePoll(() => (withSleep ? agent.sleep() : Promise.resolve(null)), withSleep ? 60_000 : 0, [settings.agent_url, withSleep]);
  const refreshSleep = sleep.refresh;
  useEffect(() => subscribe("sleep", () => void refreshSleep()), [subscribe, refreshSleep]);
  const asleepKey = (sleep.data?.targets ?? [])
    .filter((t) => t.state !== "awake" && t.pull_request_id === 0)
    .map((t) => t.app_uuid)
    .sort()
    .join(",");
  const asleep = useMemo(() => new Set(asleepKey ? asleepKey.split(",") : []), [asleepKey]);

  // Menu-bar item (desktop app): the selected server at a glance.
  const tray = trayText(settings.hostname, !system.error, openProblems(alerts.data?.alerts ?? []));
  useEffect(() => {
    if (!inTauri) return;
    invoke("tray_status", { text: tray }).catch(() => {});
  }, [tray]);

  return <Ctx.Provider value={{ system, apps, health, alerts, jobs, streaming, asleep, subscribe }}>{children}</Ctx.Provider>;
}

export function useLive(): Live {
  const v = useContext(Ctx);
  if (!v) throw new Error("useLive outside LiveProvider");
  return v;
}

/** Run `fn` whenever the server reports `topic` changed. */
export function useLiveTopic(topic: string, fn: () => void) {
  const { subscribe } = useLive();
  const ref = useRef(fn);
  ref.current = fn;
  useEffect(() => subscribe(topic, () => ref.current()), [subscribe, topic]);
}
