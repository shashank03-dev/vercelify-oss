import { AnimatePresence, motion } from "framer-motion";
import {
  Box,
  Check,
  ChevronsUpDown,
  CircleAlert,
  Container,
  Globe,
  Layers,
  Moon,
  Package,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Search,
  Server,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { isWorker } from "../lib/api";
import { pct, statusHealth } from "../lib/format";
import { useLive } from "../lib/live";
import { NAV, NAV_GROUPS, useNav, type NavItem } from "../lib/nav";
import { agentHost, useSession } from "../lib/session";
import type { SystemSnapshot } from "../types";
import { useToast } from "./Toast";
import { cx, Kbd, Logo, Tooltip } from "./ui";

export type { Tab } from "../lib/nav";

const MOD = typeof navigator !== "undefined" && /Mac/.test(navigator.platform) ? "⌘" : "Ctrl";
const COLLAPSED_KEY = "vercelify.sidebar.collapsed";

function loadCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSED_KEY) === "1";
  } catch {
    return false;
  }
}

/**
 * A floating card beside the page panel (after morphin.dev's admin dashboard):
 * brand and collapse toggle on top, a filled search field, grouped nav whose
 * current page is a solid tile, your apps as quick links, and at the bottom a
 * dark-mode switch and the server card with its vitals. Collapses to an icon
 * rail with tooltips; the choice is remembered.
 */
export function Sidebar() {
  const { tab, go, openPalette, theme, setTheme } = useNav();
  const [collapsed, setCollapsed] = useState(loadCollapsed);
  const { system: ping, apps, asleep } = useLive();
  const online = !!ping.data && !ping.error;
  const list = [...(apps.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  const dark = theme === "dark" || (theme === "system" && !window.matchMedia?.("(prefers-color-scheme: light)").matches);

  const toggle = () =>
    setCollapsed((c) => {
      try {
        localStorage.setItem(COLLAPSED_KEY, c ? "0" : "1");
      } catch {
        /* private mode */
      }
      return !c;
    });

  const item = (i: NavItem, n: number) => {
    const active = tab === i.id;
    const Icon = i.icon;
    const count = i.id === "apps" && list.length ? list.length : undefined;
    return (
      <Tooltip key={i.id} content={<NavTip label={collapsed ? i.label : undefined} hint={i.hint} keys={`${MOD}${n}`} />}>
        <button
          type="button"
          data-testid={`nav-${i.id}`}
          aria-current={active ? "page" : undefined}
          aria-label={collapsed ? i.label : undefined}
          onClick={() => go(i.id)}
          className={cx(
            "group relative isolate flex h-9 w-full items-center gap-2.5 rounded-lg text-left text-[13px] transition-colors",
            collapsed ? "justify-center px-0" : "px-2.5",
            active ? "font-medium text-inverse" : "text-fg-muted hover:bg-fg/[0.05] hover:text-fg",
          )}
        >
          {active && (
            <motion.span
              layoutId="nav-active"
              className="absolute inset-0 -z-10 rounded-lg bg-fg shadow-[0_1px_0_0_rgb(255_255_255/0.2)_inset,0_6px_16px_-8px_rgb(0_0_0/0.5)]"
              transition={{ type: "spring", stiffness: 520, damping: 40 }}
            />
          )}
          <Icon className={cx("size-[17px] shrink-0", active ? "text-inverse" : "text-fg-dim group-hover:text-fg-muted")} />
          {!collapsed && (
            <>
              <span className="flex-1 truncate">{i.label}</span>
              <span className={cx("text-[11px] tabular-nums", active ? "text-inverse/60" : "text-fg-dim")}>{count}</span>
            </>
          )}
        </button>
      </Tooltip>
    );
  };

  return (
    <motion.aside
      animate={{ width: collapsed ? 72 : 252 }}
      initial={false}
      transition={{ type: "spring", stiffness: 420, damping: 40 }}
      className="flex shrink-0 flex-col overflow-hidden rounded-xl bg-panel shadow-panel"
    >
      {/* macOS traffic lights sit here; this strip drags the window. */}
      <div data-tauri-drag-region className="h-9 shrink-0" />

      <div className={cx("flex items-center gap-2.5 px-3.5", collapsed && "flex-col px-0")} data-tauri-drag-region>
        <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-line-strong bg-gradient-to-b from-surface-3 to-surface">
          <Logo className="h-3.5 w-auto" />
        </span>
        {!collapsed && (
          <span className="min-w-0 flex-1 leading-tight">
            <span className="block text-[14px] font-semibold tracking-[-0.02em]">Vercelify</span>
            <span className="block truncate text-[11px] text-fg-dim">Your own cloud</span>
          </span>
        )}
        <Tooltip content={collapsed ? "Expand sidebar" : "Collapse sidebar"}>
          <button
            type="button"
            onClick={toggle}
            aria-label={collapsed ? "Expand sidebar" : "Collapse sidebar"}
            className="flex size-7 items-center justify-center rounded-md text-fg-dim transition hover:bg-fg/[0.06] hover:text-fg"
          >
            {collapsed ? <PanelLeftOpen className="size-4" /> : <PanelLeftClose className="size-4" />}
          </button>
        </Tooltip>
      </div>

      <div className={cx("mt-3.5", collapsed ? "px-3" : "px-3")}>
        <Rail collapsed={collapsed} label={`Search  ${MOD}K`}>
          <button
            type="button"
            onClick={openPalette}
            data-testid="open-palette"
            aria-label="Search"
            className={cx(
              "flex h-9 w-full items-center gap-2 rounded-lg bg-fg/[0.05] text-[13px] text-fg-dim transition hover:bg-fg/[0.08] hover:text-fg-muted",
              collapsed ? "justify-center" : "px-2.5",
            )}
          >
            <Search className="size-4 shrink-0" />
            {!collapsed && (
              <>
                <span className="flex-1 text-left">Search…</span>
                <Kbd className="h-[18px] border-0 bg-fg/[0.07] text-[10px]">{MOD}K</Kbd>
              </>
            )}
          </button>
        </Rail>
      </div>

      <nav className="mt-4 flex min-h-0 flex-1 flex-col gap-4 overflow-x-hidden overflow-y-auto px-3 pb-2">
        {NAV_GROUPS.map((g) => (
          <div key={g} className="flex flex-col gap-0.5">
            {collapsed ? <div className="mx-auto mb-1 h-px w-5 bg-line" /> : <div className="px-2.5 pb-1 text-[11px] font-medium text-fg-dim">{g}</div>}
            {NAV.filter((n) => n.group === g).map((n) => item(n, NAV.indexOf(n) + 1))}
          </div>
        ))}

        {!collapsed && list.length > 0 && (
          <div className="flex flex-col gap-0.5">
            <div className="flex items-center justify-between px-2.5 pb-1 text-[11px] font-medium text-fg-dim">
              Your apps
              <Tooltip content="New app">
                <button
                  type="button"
                  aria-label="New app"
                  onClick={() => go("apps", { newApp: true })}
                  className="rounded p-0.5 text-fg-dim transition hover:bg-fg/[0.06] hover:text-fg"
                >
                  <Plus className="size-3.5" />
                </button>
              </Tooltip>
            </div>
            {list.slice(0, 8).map((a) => (
              <button
                key={a.uuid}
                type="button"
                onClick={() => go("apps", { app: a.uuid })}
                className="group flex h-8 w-full items-center gap-2.5 rounded-lg px-2.5 text-left text-[13px] text-fg-muted transition-colors hover:bg-fg/[0.05] hover:text-fg"
              >
                <AppIcon pack={a.build_pack} />
                <span className="min-w-0 flex-1 truncate">{a.name}</span>
                {statusHealth(a.status) === "bad" && !asleep.has(a.uuid) && (
                  <Tooltip content="Stopped or failing">
                    <CircleAlert className="size-3.5 shrink-0 text-bad" aria-label="needs attention" />
                  </Tooltip>
                )}
              </button>
            ))}
          </div>
        )}
      </nav>

      <div className="flex flex-col gap-1 border-t border-line px-3 pt-2 pb-3">
        {NAV.filter((n) => n.group === "footer").map((n) => item(n, NAV.indexOf(n) + 1))}
        <Rail collapsed={collapsed} label={dark ? "Light mode" : "Dark mode"}>
          <button
            type="button"
            role="switch"
            aria-checked={dark}
            aria-label="Dark mode"
            onClick={() => setTheme(dark ? "light" : "dark")}
            className={cx(
              "flex h-9 w-full items-center gap-2.5 rounded-lg text-[13px] text-fg-muted transition hover:bg-fg/[0.05] hover:text-fg",
              collapsed ? "justify-center" : "px-2.5",
            )}
          >
            <Moon className="size-[17px] shrink-0 text-fg-dim" />
            {!collapsed && (
              <>
                <span className="flex-1 text-left">Dark mode</span>
                <Switch on={dark} />
              </>
            )}
          </button>
        </Rail>
        <ServerCard collapsed={collapsed} online={online} loading={ping.loading} snap={ping.data} />
      </div>
    </motion.aside>
  );
}

/** A page's tooltip: its name when the rail hides it, what it's for, its shortcut. */
function NavTip({ label, hint, keys }: { label?: string; hint: string; keys: string }) {
  return (
    <span className="flex items-center gap-2">
      <span>
        {label && <span className="font-medium">{label} · </span>}
        <span className={label ? "text-fg-muted" : undefined}>{hint}</span>
      </span>
      <Kbd className="h-4 border-0 bg-fg/[0.08] text-[10px]">{keys}</Kbd>
    </span>
  );
}

/** Wraps a sidebar control so it gets a tooltip only when the labels are hidden. */
function Rail({ collapsed, label, children }: { collapsed: boolean; label: string; children: React.ReactElement<Record<string, unknown>> }) {
  return collapsed ? <Tooltip content={label}>{children}</Tooltip> : children;
}

function Switch({ on }: { on: boolean }) {
  return (
    <span className={cx("relative inline-flex h-[18px] w-[30px] shrink-0 rounded-full transition-colors duration-200", on ? "bg-fg" : "bg-fg/[0.15]")}>
      <motion.span
        className="absolute top-[2px] size-[14px] rounded-full bg-panel shadow-sm"
        animate={{ left: on ? 14 : 2 }}
        transition={{ type: "spring", stiffness: 600, damping: 34 }}
      />
    </span>
  );
}

/** What the app is built from, so the list reads at a glance without status dots. */
function AppIcon({ pack }: { pack: string }) {
  const Icon = pack === "static" ? Globe : pack === "dockerimage" ? Container : pack === "dockercompose" ? Layers : pack === "dockerfile" ? Box : Package;
  return <Icon className="size-4 shrink-0 text-fg-dim group-hover:text-fg-muted" />;
}

function MiniMeter({ label, value }: { label: string; value: number }) {
  return (
    <span className="flex min-w-0 flex-1 flex-col gap-1">
      <span className="flex justify-between text-[10px] text-fg-dim">
        {label}
        <span className="tabular-nums">{Math.round(value)}%</span>
      </span>
      <span className="h-[3px] overflow-hidden rounded-full bg-fg/[0.08]">
        <span
          className={cx("block h-full rounded-full transition-[width] duration-500", value > 90 ? "bg-bad" : value > 75 ? "bg-warn" : "bg-fg-muted")}
          style={{ width: `${Math.max(3, value)}%` }}
        />
      </span>
    </span>
  );
}

/**
 * The selected server, like the account card at the foot of a dashboard
 * sidebar: name, how it's reached and its CPU / memory / disk. Click to switch
 * servers or add one.
 */
function ServerCard({ collapsed, online, loading, snap }: { collapsed: boolean; online: boolean; loading: boolean; snap?: SystemSnapshot }) {
  const { settings, servers, select, addServer } = useSession();
  const { go } = useNav();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const disk = snap?.disks.find((d) => d.role === "system") ?? snap?.disks[0];

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => ref.current && !ref.current.contains(e.target as Node) && setOpen(false);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const tile = (
    <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-fg/[0.07] text-fg-muted">
      <Server className="size-4" />
    </span>
  );
  const name = snap?.host.hostname ?? settings.hostname;

  return (
    <div className="relative" ref={ref}>
      <AnimatePresence>
        {open && (
          <motion.div
            initial={{ opacity: 0, y: 6, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 6, scale: 0.98 }}
            transition={{ duration: 0.14 }}
            role="menu"
            data-testid="server-menu"
            className={cx(
              "absolute bottom-full z-40 mb-1.5 w-[228px] origin-bottom-left overflow-hidden rounded-xl border border-line-strong bg-overlay p-1 shadow-lg",
              "left-0",
            )}
          >
            <div className="px-2 pt-1 pb-1.5 text-[11px] font-medium text-fg-dim">Servers</div>
            {servers.map((s) => {
              const current = s.agent_url === settings.agent_url;
              return (
                <MenuItem
                  key={s.agent_url}
                  testId={`switch-${s.hostname}`}
                  icon={<Server />}
                  onClick={() => {
                    setOpen(false);
                    if (!current) void select(s.agent_url).then(() => toast.info(`Switched to ${s.hostname}`), (e: Error) => toast.bad(e.message));
                  }}
                >
                  <span className="min-w-0 flex-1 truncate">{s.hostname}</span>
                  {isWorker(s) && <span className="text-[10px] text-fg-dim">worker</span>}
                  {current && <Check className="size-3.5 text-accent" />}
                </MenuItem>
              );
            })}
            <div className="my-1 h-px bg-line" />
            <MenuItem testId="add-server" icon={<Plus />} onClick={() => (setOpen(false), addServer())}>
              Add a server…
            </MenuItem>
            <MenuItem icon={<Server />} onClick={() => (setOpen(false), go("servers"))}>
              All servers
            </MenuItem>
          </motion.div>
        )}
      </AnimatePresence>
      <Rail collapsed={collapsed} label={name}>
        <button
          type="button"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
          className={cx(
            "w-full rounded-xl text-left transition",
            collapsed ? "flex justify-center py-1" : "bg-fg/[0.04] p-2.5 hover:bg-fg/[0.07]",
            open && !collapsed && "bg-fg/[0.07]",
          )}
          data-testid="host-status"
        >
          {collapsed ? (
            tile
          ) : (
            <>
              <span className="flex items-center gap-2.5">
                {tile}
                <span className="min-w-0 flex-1 leading-tight">
                  <span className="block truncate text-[13px] font-medium">{name}</span>
                  <span className={cx("block truncate text-[11px]", online || loading ? "text-fg-dim" : "text-bad")} title={settings.agent_url}>
                    {online ? `via ${agentHost(settings)}` : loading ? "connecting…" : "unreachable"}
                  </span>
                </span>
                <ChevronsUpDown className="size-3.5 shrink-0 text-fg-dim" />
              </span>
              {snap && (
                <span className="mt-2.5 flex gap-2.5" data-testid="sidebar-vitals">
                  <MiniMeter label="CPU" value={snap.cpu.usage_pct} />
                  <MiniMeter label="Mem" value={pct(snap.memory.used_bytes, snap.memory.total_bytes)} />
                  {disk && <MiniMeter label="Disk" value={pct(disk.used_bytes, disk.total_bytes)} />}
                </span>
              )}
            </>
          )}
        </button>
      </Rail>
    </div>
  );
}

function MenuItem({ children, icon, onClick, testId }: { children: ReactNode; icon: ReactNode; onClick: () => void; testId?: string }) {
  return (
    <button
      type="button"
      role="menuitem"
      data-testid={testId}
      onClick={onClick}
      className="flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] text-fg-muted hover:bg-fg/[0.06] hover:text-fg [&>svg]:size-3.5 [&>svg]:shrink-0 [&>svg]:text-fg-dim"
    >
      {icon}
      {children}
    </button>
  );
}
