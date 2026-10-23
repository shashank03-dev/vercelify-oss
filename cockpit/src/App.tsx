import { AnimatePresence, motion } from "framer-motion";
import { useCallback, useEffect, useMemo, useState } from "react";
import { CommandPalette } from "./components/CommandPalette";
import { AddServerWizard } from "./components/AddServer";
import { Sidebar } from "./components/Sidebar";
import { TopBar } from "./components/TopBar";
import { ToastProvider, useToast } from "./components/Toast";
import { Logo, Modal, PixelWave } from "./components/ui";
import { isWorker, servers as serversApi, settings as settingsApi } from "./lib/api";
import { isTab, NAV, NavProvider, type Nav, type Tab } from "./lib/nav";
import { LiveProvider } from "./lib/live";
import { SessionProvider } from "./lib/session";
import { useTheme } from "./lib/theme";
import { configOps } from "./lib/transport";
import type { CockpitConfig, CockpitSettings } from "./types";
import { AiView } from "./views/AiView";
import { AppsView } from "./views/AppsView";
import { HealthView } from "./views/HealthView";
import { Onboarding } from "./views/Onboarding";
import { ScreencastView } from "./views/ScreencastView";
import { ServersView } from "./views/ServersView";
import { SettingsView } from "./views/SettingsView";
import { StorageView } from "./views/StorageView";

const TAB_KEY = "vercelify.tab";

function initialTab(): Tab {
  const fromHash = window.location.hash.replace("#", "");
  if (isTab(fromHash)) return fromHash;
  try {
    const saved = localStorage.getItem(TAB_KEY) ?? "";
    return isTab(saved) ? saved : "apps";
  } catch {
    return "apps";
  }
}

export default function App() {
  return (
    <ToastProvider>
      <Shell />
    </ToastProvider>
  );
}

function Shell() {
  const [config, setConfig] = useState<CockpitConfig | undefined>(undefined);
  const [tab, setTab] = useState<Tab>(initialTab);
  const [intent, setIntent] = useState<Nav["intent"]>(null);
  const [palette, setPalette] = useState(false);
  const [adding, setAdding] = useState(false);
  const [revision, setRevision] = useState(0);
  const [theme, setTheme] = useTheme();
  const toast = useToast();

  const reload = useCallback(
    () =>
      serversApi.load().then(
        (c) => (setConfig(c), setRevision((r) => r + 1)),
        () => setConfig({ servers: [], active: null }),
      ),
    [],
  );
  const closeWizard = useCallback(() => {
    setAdding(false);
    setRevision((r) => r + 1); // a worker may have joined a pool
  }, []);
  useEffect(() => {
    void reload();
  }, [reload]);

  const active = config ? configOps.active(config) : null;

  const go = useCallback((t: Tab, i?: Nav["intent"]) => {
    setTab(t);
    setIntent(i ?? null);
    try {
      localStorage.setItem(TAB_KEY, t);
    } catch {
      /* private mode */
    }
  }, []);

  // ⌘K palette, ⌘1…⌘7 pages.
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
      if (e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((p) => !p);
        return;
      }
      const n = Number(e.key);
      if (n >= 1 && n <= NAV.length) {
        e.preventDefault();
        go(NAV[n - 1]!.id);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [active, go]);

  const nav = useMemo<Nav>(
    () => ({
      tab,
      go,
      intent,
      clearIntent: () => setIntent(null),
      openPalette: () => setPalette(true),
      theme,
      setTheme,
    }),
    [tab, go, intent, theme, setTheme],
  );

  const onPaired = useCallback(
    async (s: CockpitSettings, note?: string) => {
      setAdding(false);
      await reload();
      if (note) toast.bad(note);
      else toast.ok(`Connected to ${s.hostname}`);
    },
    [reload, toast],
  );

  const session = useMemo(
    () =>
      active && config
        ? {
            settings: active,
            servers: config.servers,
            unpair: async () => {
              await settingsApi.clear();
              await reload();
            },
            update: async (s: CockpitSettings) => {
              // Re-pairing can change the agent URL: drop the old entry.
              if (s.agent_url !== active.agent_url) await serversApi.remove(active.agent_url);
              await settingsApi.save(s);
              await reload();
            },
            select: async (agentUrl: string) => {
              setConfig(await serversApi.select(agentUrl));
            },
            remove: async (agentUrl: string) => {
              setConfig(await serversApi.remove(agentUrl));
            },
            addServer: () => setAdding(true),
            revision,
          }
        : null,
    [active, config, reload, revision],
  );

  return (
    <NavProvider value={nav}>
      {config === undefined ? (
        <div className="flex h-full flex-col items-center justify-center gap-5" data-tauri-drag-region>
          <Logo className="h-7 w-auto" />
          <PixelWave size={4} pixel={4} gap={2} />
        </div>
      ) : !session ? (
        <Onboarding onPaired={onPaired} />
      ) : (
        <SessionProvider value={session}>
          <LiveProvider key={session.settings.agent_url}>
            {/* Two floating cards on the window chrome: the sidebar and the page. */}
            <div className="flex h-full gap-2 bg-bg p-2">
              <Sidebar key={session.settings.agent_url} />
              <div className="flex min-w-0 flex-1">
                <main className="flex min-w-0 flex-1 flex-col overflow-hidden rounded-xl bg-panel shadow-panel">
                  <TopBar />
                  <div className="min-h-0 flex-1 overflow-y-auto">
                    <AnimatePresence mode="wait" initial={false}>
                      <motion.div
                        // A different server is a different set of data: remount every page.
                        key={`${session.settings.agent_url}|${tab}`}
                        initial={{ opacity: 0, y: 4 }}
                        animate={{ opacity: 1, y: 0 }}
                        exit={{ opacity: 0 }}
                        transition={{ duration: 0.14, ease: [0.22, 1, 0.36, 1] }}
                        className="mx-auto max-w-[1240px] px-7 pt-6 pb-14"
                      >
                        {tab === "apps" && <AppsView />}
                        {tab === "ai" && <AiView />}
                        {tab === "servers" && <ServersView />}
                        {tab === "storage" && <StorageView />}
                        {tab === "screencast" && <ScreencastView />}
                        {tab === "health" && <HealthView />}
                        {tab === "settings" && <SettingsView />}
                      </motion.div>
                    </AnimatePresence>
                  </div>
                </main>
              </div>
            </div>
            <CommandPalette open={palette} onClose={() => setPalette(false)} />
            <Modal
              open={adding}
              onClose={closeWizard}
              title="Add a machine"
              description="Any old laptop, PC or Raspberry Pi. It takes one command on the machine and an 8-character code here."
            >
              <AddServerWizard
                servers={config.servers}
                onClose={closeWizard}
                onAdded={async (s) => {
                  // A worker is managed from its main server: stay where we were.
                  if (isWorker(s) && active && active.agent_url !== s.agent_url) await serversApi.select(active.agent_url).catch(() => {});
                  await reload();
                }}
              />
            </Modal>
          </LiveProvider>
        </SessionProvider>
      )}
    </NavProvider>
  );
}
