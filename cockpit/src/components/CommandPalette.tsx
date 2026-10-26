import { Command } from "cmdk";
import { AnimatePresence, motion } from "framer-motion";
import { ArrowRightLeft, ExternalLink, Monitor, Moon, Plus, Rocket, Search, Server, Sun, SquareArrowOutUpRight } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { isWorker } from "../lib/api";
import { hostOf } from "../lib/format";
import { NAV, useNav } from "../lib/nav";
import { openExternal } from "../lib/open";
import { listedApp, platform, type ListedApp } from "../lib/platform";
import { useSession } from "../lib/session";
import { useToast } from "./Toast";
import { Kbd, StatusDot } from "./ui";

const MOD = typeof navigator !== "undefined" && /Mac/.test(navigator.platform) ? "⌘" : "Ctrl";

/** ⌘K: jump anywhere, deploy or open any app, switch theme. */
export function CommandPalette({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { go, setTheme } = useNav();
  const { settings, servers, select, addServer } = useSession();
  const toast = useToast();
  const [apps, setApps] = useState<ListedApp[]>();
  const [search, setSearch] = useState("");

  useEffect(() => {
    if (!open) return;
    setSearch("");
    if (isWorker(settings)) setApps([]);
    else platform.apps().then((l) => setApps(l.map(listedApp)), () => setApps([]));
  }, [open, settings]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  const run = (fn: () => unknown) => () => {
    onClose();
    void fn();
  };

  const deploy = (a: ListedApp) =>
    platform.deploy(a.id).then(
      () => toast.ok(`Deployment of ${a.name} queued`),
      (e: Error) => toast.bad(e.message),
    );

  return (
    <AnimatePresence>
      {open && (
        <motion.div
          className="fixed inset-0 z-[70] flex items-start justify-center bg-black/45 px-4 pt-[14vh] backdrop-blur-[2px]"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.12 }}
          onMouseDown={(e) => e.target === e.currentTarget && onClose()}
        >
          <motion.div
            initial={{ y: -8, scale: 0.985, opacity: 0 }}
            animate={{ y: 0, scale: 1, opacity: 1 }}
            exit={{ y: -4, scale: 0.985, opacity: 0 }}
            transition={{ type: "spring", stiffness: 520, damping: 38 }}
            className="w-full max-w-[600px] overflow-hidden rounded-xl border border-line-strong bg-overlay shadow-lg"
          >
            <Command label="Command palette" loop className="[&_[cmdk-group-heading]]:px-3 [&_[cmdk-group-heading]]:pt-3 [&_[cmdk-group-heading]]:pb-1.5 [&_[cmdk-group-heading]]:text-[11px] [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-fg-dim">
              <div className="flex items-center gap-2.5 border-b border-line px-4">
                <Search className="size-4 text-fg-dim" />
                <Command.Input
                  autoFocus
                  value={search}
                  onValueChange={setSearch}
                  placeholder="Search apps, pages and actions…"
                  className="h-12 flex-1 bg-transparent text-[14px] text-fg outline-none placeholder:text-fg-dim focus-visible:outline-none"
                  data-testid="palette-input"
                />
                <Kbd>esc</Kbd>
              </div>
              <Command.List className="max-h-[min(420px,55vh)] scroll-py-2 overflow-y-auto overscroll-contain px-1.5 pb-1.5">
                <Command.Empty className="py-10 text-center text-[13px] text-fg-muted">No results for “{search}”.</Command.Empty>

                <Command.Group heading="Go to">
                  {NAV.map((n, i) => {
                    const Icon = n.icon;
                    return (
                      <Item key={n.id} value={`go ${n.label} ${n.hint}`} icon={<Icon />} onSelect={run(() => go(n.id))} right={<Kbd>{MOD}{i + 1}</Kbd>}>
                        {n.label} <span className="text-fg-dim">— {n.hint}</span>
                      </Item>
                    );
                  })}
                </Command.Group>

                <Command.Group heading="Create">
                  <Item value="new app deploy git repository branch" icon={<Plus />} onSelect={run(() => go("apps", { newApp: true }))}>
                    New app from Git
                  </Item>
                  <Item value="add server laptop machine pair raspberry pi" icon={<Server />} onSelect={run(addServer)}>
                    Add a machine…
                  </Item>
                </Command.Group>

                {!!apps?.length && (
                  <Command.Group heading="Apps">
                    {apps.map((a) => {
                      const url = a.url;
                      return [
                        <Item key={`open-${a.id}`} value={`open ${a.name} deployments logs`} icon={<SquareArrowOutUpRight />} onSelect={run(() => go("apps", { app: a.id }))}>
                          {a.name} <span className="text-fg-dim">— deployments & logs</span>
                        </Item>,
                        url && (
                          <Item key={`visit-${a.id}`} value={`visit ${a.name} ${hostOf(url)}`} icon={<ExternalLink />} onSelect={run(() => openExternal(url))}>
                            Visit <span className="font-mono text-[12px]">{hostOf(url)}</span>
                          </Item>
                        ),
                      ];
                    })}
                  </Command.Group>
                )}

                {!!apps?.length && (
                  <Command.Group heading="Deploy">
                    {apps.map((a) => (
                      <Item
                        key={`deploy-${a.id}`}
                        value={`deploy ${a.name}`}
                        icon={<Rocket />}
                        onSelect={run(() => deploy(a))}
                        right={<StatusDot health={a.health} />}
                      >
                        Deploy <span className="font-medium text-fg">{a.name}</span>
                      </Item>
                    ))}
                  </Command.Group>
                )}

                {servers.length > 1 && (
                  <Command.Group heading="Servers">
                    {servers
                      .filter((s) => s.agent_url !== settings.agent_url)
                      .map((s) => (
                        <Item
                          key={s.agent_url}
                          value={`switch server ${s.hostname}`}
                          icon={<ArrowRightLeft />}
                          onSelect={run(() => select(s.agent_url).then(() => toast.info(`Switched to ${s.hostname}`), (e: Error) => toast.bad(e.message)))}
                        >
                          Switch to <span className="font-medium text-fg">{s.hostname}</span>
                        </Item>
                      ))}
                  </Command.Group>
                )}

                <Command.Group heading="Appearance">
                  <Item value="theme dark" icon={<Moon />} onSelect={run(() => setTheme("dark"))}>
                    Dark theme
                  </Item>
                  <Item value="theme light" icon={<Sun />} onSelect={run(() => setTheme("light"))}>
                    Light theme
                  </Item>
                  <Item value="theme system" icon={<Monitor />} onSelect={run(() => setTheme("system"))}>
                    Match system theme
                  </Item>
                </Command.Group>
              </Command.List>
            </Command>
          </motion.div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}

function Item({
  value,
  icon,
  children,
  onSelect,
  right,
}: {
  value: string;
  icon: ReactNode;
  children: ReactNode;
  onSelect: () => void;
  right?: ReactNode;
}) {
  return (
    <Command.Item
      value={value}
      onSelect={onSelect}
      className="flex h-9 cursor-pointer items-center gap-2.5 rounded-lg px-2.5 text-[13px] text-fg-muted data-[selected=true]:bg-fg/[0.07] data-[selected=true]:text-fg [&_svg]:size-4 [&>svg]:text-fg-dim"
    >
      {icon}
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {right}
    </Command.Item>
  );
}
