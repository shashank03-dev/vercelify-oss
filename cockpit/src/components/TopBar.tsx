import { AnimatePresence, motion, type Variants } from "framer-motion";
import { AlertTriangle, BatteryLow, Bell, CheckCircle2, ChevronRight, ChevronUp, HardDrive, Info, Loader2, Thermometer, type LucideIcon } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { ago, pct } from "../lib/format";
import { useLive } from "../lib/live";
import { NAV, useNav } from "../lib/nav";
import { useSession } from "../lib/session";
import { useActivity } from "./Toast";
import { cx } from "./ui";

/**
 * Location bar on top of the page panel: where you are on the left, the
 * activity bell on the right. The strip doubles as the window drag region.
 */
export function TopBar() {
  const { tab } = useNav();
  const { settings } = useSession();
  const page = NAV.find((n) => n.id === tab)!;
  const Icon = page.icon;

  return (
    <header data-tauri-drag-region className="relative z-30 flex h-12 shrink-0 items-center gap-3 border-b border-line px-4">
      <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1.5 text-[13px]" data-tauri-drag-region>
        <span className="truncate text-fg-muted">{settings.hostname}</span>
        <ChevronRight className="size-3.5 shrink-0 text-fg-dim" />
        <span className="flex items-center gap-1.5 font-medium text-fg">
          <Icon className="size-3.5 text-fg-muted" />
          {page.label}
        </span>
      </nav>
      <div className="flex-1 self-stretch" data-tauri-drag-region />
      <ActivityMenu />
    </header>
  );
}

interface Item {
  key: string;
  icon: LucideIcon;
  tone: "ok" | "bad" | "warn" | "idle";
  title: string;
  detail?: string;
  when?: string;
  onClick?: () => void;
}

const TONE: Record<Item["tone"], string> = { ok: "text-ok", bad: "text-bad", warn: "text-warn", idle: "text-fg-muted" };

const list: Variants = { open: { transition: { staggerChildren: 0.045, delayChildren: 0.08 } }, closed: {} };
const row: Variants = {
  open: { opacity: 1, y: 0, filter: "blur(0px)", transition: { type: "spring", stiffness: 420, damping: 32 } },
  closed: { opacity: 0, y: 8, filter: "blur(3px)" },
};

/**
 * After morphin.dev's activity dropdown: the bell button itself grows into
 * the panel (one shared layout, so it morphs rather than pops), the bell
 * slides into the panel's header and the entries fade in one after another.
 * "Needs attention" is worked out from live data; "Recent" is what this
 * session told you.
 */
function ActivityMenu() {
  const [open, setOpen] = useState(false);
  const { events, unread, markRead } = useActivity();
  const { system, apps, alerts: serverAlerts, jobs, asleep } = useLive();
  const { go } = useNav();
  const { settings } = useSession();
  const ref = useRef<HTMLDivElement>(null);

  const alerts = useMemo<Item[]>(() => {
    const out: Item[] = [];
    for (const a of apps.data ?? []) {
      if (a.health !== "bad" || asleep.has(a.id)) continue;
      out.push({
        key: `app-${a.id}`,
        icon: AlertTriangle,
        tone: "bad",
        title: `${a.name} is not running: its deploy failed`,
        detail: "Open its deployments and logs",
        onClick: () => go("apps", { app: a.id }),
      });
    }
    // Problems the server itself reported (power supply, disks, deploys, uptime…):
    // the newest alert per source, while it isn't resolved.
    const seen = new Set<string>();
    for (const a of serverAlerts.data?.alerts ?? []) {
      if (seen.has(a.source)) continue;
      seen.add(a.source);
      if (a.resolved || a.status === "ok") continue;
      out.push({
        key: `alert-${a.id}`,
        icon: AlertTriangle,
        tone: a.status === "bad" ? "bad" : "warn",
        title: a.title,
        detail: a.detail,
        when: ago(a.at),
        onClick: () => go("settings"),
      });
      if (seen.size > 12) break;
    }
    const s = system.data;
    if (s) {
      for (const d of s.disks) {
        const used = pct(d.used_bytes, d.total_bytes);
        if (used > 85)
          out.push({ key: `disk-${d.mount_point}`, icon: HardDrive, tone: "warn", title: `${d.mount_point} is ${Math.round(used)}% full`, detail: "Move volumes to the other disk", onClick: () => go("storage") });
      }
      if (s.power.on_battery)
        out.push({ key: "battery", icon: BatteryLow, tone: "warn", title: `On battery · ${Math.round(s.power.lowest_capacity_pct ?? 0)}%`, detail: "The power guard pauses deploys when it runs low", onClick: () => go("health") });
      if ((s.thermal.max_c ?? 0) >= 85)
        out.push({ key: "heat", icon: Thermometer, tone: "warn", title: `Running hot · ${Math.round(s.thermal.max_c ?? 0)}°C`, detail: "Check the fan and airflow", onClick: () => go("health") });
    }
    return out;
  }, [apps.data, system.data, serverAlerts.data, asleep, go]);

  // Work in progress on the server (backups, moves, downloads, deploys).
  const running: Item[] = jobs
    .filter((j) => j.status === "running")
    .slice(0, 5)
    .map((j) => ({
      key: `job-${j.id}`,
      icon: Loader2,
      tone: "idle",
      title: j.title,
      detail: [j.progress != null ? `${Math.round(j.progress)}%` : null, j.detail].filter(Boolean).join(" · ") || undefined,
      when: ago(j.started_at),
      onClick: () => go("health"),
    }));

  const recent: Item[] = events.slice(0, 8).map((e) => ({
    key: `ev-${e.id}`,
    icon: e.tone === "ok" ? CheckCircle2 : e.tone === "bad" ? AlertTriangle : Info,
    tone: e.tone === "info" ? "idle" : e.tone,
    title: e.text,
    when: ago(new Date(e.at).toISOString()),
  }));

  const count = alerts.length + unread;

  useEffect(() => {
    if (!open) return;
    markRead();
    const onDown = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
    // markRead changes with every event; reading once on open is enough.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const spring = { type: "spring", stiffness: 380, damping: 34 } as const;

  return (
    <div ref={ref} className="relative size-8">
      <AnimatePresence initial={false}>
        {!open ? (
          <motion.button
            key="bell"
            type="button"
            layoutId="activity"
            onClick={() => setOpen(true)}
            aria-label={count ? `Activity, ${count} new` : "Activity"}
            data-testid="activity-button"
            className="absolute inset-0 flex items-center justify-center bg-fg/[0.05] text-fg-muted hover:bg-fg/[0.08] hover:text-fg"
            style={{ borderRadius: 9 }}
            transition={spring}
          >
            <motion.span layoutId="activity-bell" className="flex" transition={spring}>
              <Bell className="size-4" />
            </motion.span>
            {count > 0 && (
              <motion.span
                initial={{ scale: 0 }}
                animate={{ scale: 1 }}
                className="absolute -top-1 -right-1 flex h-4 min-w-4 items-center justify-center rounded-full bg-fg px-1 text-[10px] font-semibold text-inverse tabular-nums"
              >
                {count}
              </motion.span>
            )}
          </motion.button>
        ) : (
          <motion.div
            key="panel"
            layoutId="activity"
            role="dialog"
            aria-label="Activity"
            data-testid="activity-panel"
            className="absolute top-0 right-0 z-50 w-[384px] overflow-hidden border border-line-strong bg-overlay shadow-lg"
            style={{ borderRadius: 14 }}
            transition={spring}
          >
            <button type="button" onClick={() => setOpen(false)} className="flex w-full items-center gap-3 px-3 pt-3 pb-2 text-left">
              <motion.span layoutId="activity-bell" className="flex size-9 shrink-0 items-center justify-center rounded-[10px] bg-fg/[0.06] text-fg-muted" transition={spring}>
                <Bell className="size-4" />
              </motion.span>
              <motion.span className="min-w-0 flex-1 leading-tight" initial={{ opacity: 0, x: -6 }} animate={{ opacity: 1, x: 0 }} transition={{ delay: 0.06 }}>
                <span className="block text-[13px] font-semibold">{count ? `${count} new ${count === 1 ? "activity" : "activities"}` : "All caught up"}</span>
                <span className="block truncate text-[11px] text-fg-dim">What's happening on {settings.hostname}</span>
              </motion.span>
              <ChevronUp className="size-4 text-fg-dim" />
            </button>
            <motion.div variants={list} initial="closed" animate="open" className="max-h-[420px] overflow-y-auto px-1.5 pb-1.5">
              {alerts.length > 0 && <Heading>Needs attention</Heading>}
              {alerts.map((i) => (
                <Entry key={i.key} item={i} close={() => setOpen(false)} />
              ))}
              {running.length > 0 && <Heading>Running</Heading>}
              {running.map((i) => (
                <Entry key={i.key} item={i} close={() => setOpen(false)} />
              ))}
              {recent.length > 0 && <Heading>Recent</Heading>}
              {recent.map((i) => (
                <Entry key={i.key} item={i} close={() => setOpen(false)} />
              ))}
              {alerts.length + running.length + recent.length === 0 && (
                <motion.p variants={row} className="px-3 pt-2 pb-5 text-[12px] text-fg-dim">
                  Nothing needs you. Deploys, restarts and alerts from this machine show up here.
                </motion.p>
              )}
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
}

function Heading({ children }: { children: string }) {
  return (
    <motion.div variants={row} className="px-2 pt-2 pb-1 text-[11px] font-medium text-fg-dim">
      {children}
    </motion.div>
  );
}

function Entry({ item: i, close }: { item: Item; close: () => void }) {
  const Icon = i.icon;
  return (
    <motion.button
      variants={row}
      type="button"
      disabled={!i.onClick}
      onClick={() => {
        i.onClick?.();
        close();
      }}
      className="flex w-full items-start gap-3 rounded-[10px] px-2 py-2 text-left transition-colors enabled:hover:bg-fg/[0.05]"
    >
      <span className={cx("flex size-8 shrink-0 items-center justify-center rounded-lg bg-fg/[0.06]", TONE[i.tone])}>
        <Icon className="size-4" />
      </span>
      <span className="min-w-0 flex-1 leading-tight">
        <span className="flex items-baseline gap-2">
          <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-fg">{i.title}</span>
          {i.when && <span className="shrink-0 text-[10.5px] text-fg-dim">{i.when}</span>}
        </span>
        {i.detail && <span className="mt-0.5 block truncate text-[12px] text-fg-muted">{i.detail}</span>}
      </span>
    </motion.button>
  );
}
