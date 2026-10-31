import { KeyRound, Laptop, LogOut, Monitor, Moon, Palette, PlugZap, Server, Sun, Wrench } from "lucide-react";
import { useState } from "react";
import { AlertsCard, AuditCard, BackupsCard, DevicesCard, DomainCard, PowerGuardCard } from "../components/ServerCare";
import { useToast } from "../components/Toast";
import { Button, Card, cx, Field, InlineCode, PageHeader, StatusDot, Tabs, TextInput } from "../components/ui";
import { local, pairingToSettings, settings as settingsApi } from "../lib/api";
import { usePoll } from "../lib/hooks";
import { useNav } from "../lib/nav";
import { useSession } from "../lib/session";
import type { ThemePref } from "../lib/theme";
import { inTauri } from "../lib/transport";
import type { CockpitSettings } from "../types";

export function SettingsView() {
  const { settings, servers, update, unpair } = useSession();
  const toast = useToast();
  const [form, setForm] = useState<CockpitSettings>(settings);
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const tools = usePoll(() => local.tools(), 0);
  const [section, setSection] = useState<"server" | "computer">("server");

  async function save(s: CockpitSettings) {
    setBusy(true);
    try {
      const res = await settingsApi.test(s);
      await update({ ...s, hostname: res.hostname || s.hostname });
      toast.ok(`Connected to ${res.hostname || s.hostname}`);
      return true;
    } catch (e) {
      toast.bad((e as Error).message);
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function repair() {
    setBusy(true);
    try {
      const joined = await settingsApi.joinPairing(code);
      const next = pairingToSettings(joined);
      // A network code is spent once joined: keep the pairing even if the
      // server doesn't answer yet (save() said why), rather than lose it.
      if ((await save(next)) || (joined.mesh_join && (await update(next).then(() => true, () => false)))) {
        setForm(next);
        setCode("");
      }
    } catch (e) {
      toast.bad((e as Error).message);
    } finally { setBusy(false); }
  }

  return (
    <>
      <PageHeader title="Settings" subtitle="What the server does on its own, and how this computer connects to it." />
      <Tabs
        className="mb-6 border-b border-line"
        label="settings section"
        value={section}
        onChange={setSection}
        items={[
          { value: "server", label: settings.hostname, icon: <Server />, testId: "settings-server" },
          { value: "computer", label: "This computer", icon: <Laptop />, testId: "settings-computer" },
        ]}
      />
      {section === "server" ? (
        <>
          <p className="mb-4 text-[13px] text-fg-muted">
            Alerts, power, backups and access. These live on {settings.hostname} and keep working with this app closed.
          </p>
          <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
            <div className="space-y-6">
              <AlertsCard />
              <PowerGuardCard />
              <DevicesCard />
            </div>
            <div className="space-y-6">
              <BackupsCard />
              <DomainCard />
              <AuditCard />
            </div>
          </div>
        </>
      ) : (
        <div className="grid grid-cols-1 gap-6 lg:grid-cols-2">
          <div className="space-y-6">
            <Card
              title="Connection"
              description={`${settings.hostname}${servers.length > 1 ? ` · one of ${servers.length} servers (switch in the sidebar)` : ""}`}
              icon={<Server />}
            >
              <form
                className="space-y-3.5"
                onSubmit={(e) => {
                  e.preventDefault();
                  void save(form);
                }}
              >
                <Field label="Agent URL">
                  <TextInput value={form.agent_url} onChange={(e) => setForm({ ...form, agent_url: e.target.value })} spellCheck={false} />
                </Field>
                <Field label="Agent token">
                  <TextInput type="password" value={form.agent_token} onChange={(e) => setForm({ ...form, agent_token: e.target.value })} />
                </Field>
                <div className="flex justify-between border-t border-line pt-3.5">
                  <Button variant="danger" icon={<LogOut />} onClick={() => void unpair()} data-testid="unpair">
                    {servers.length > 1 ? `Remove ${settings.hostname}` : "Unpair"}
                  </Button>
                  <Button type="submit" variant="primary" busy={busy} icon={<PlugZap />}>
                    Test & save
                  </Button>
                </div>
              </form>
            </Card>

            <Card title="Pair again" icon={<KeyRound />}>
              <p className="mb-3 text-[13px] text-fg-muted">
                Paste a fresh code from <InlineCode>sudo vercelify pair</InlineCode>.
              </p>
              <div className="flex gap-2">
                <TextInput placeholder="vfy2.…" value={code} onChange={(e) => setCode(e.target.value)} spellCheck={false} className="font-mono" />
                <Button onClick={() => void repair()} disabled={!code.trim()} busy={busy}>
                  Apply
                </Button>
              </div>
            </Card>
          </div>

          <div className="space-y-6">
            <Appearance />

            <Card title="Tools on this computer" icon={<Wrench />} description="Used for display streaming and remote desktop">
              {!inTauri && <p className="mb-2 text-[13px] text-fg-muted">Browser preview: local tools are managed by the desktop app.</p>}
              <div className="divide-y divide-line">
                {(tools.data ?? []).map((t) => (
                  <div key={t.name} className="flex items-center gap-3 py-2 text-[13px]">
                    <StatusDot health={t.installed ? "ok" : "warn"} />
                    <span className="w-24 font-medium">{t.name}</span>
                    {t.installed ? (
                      <span className="truncate font-mono text-xs text-fg-muted">{t.path}</span>
                    ) : (
                      <span className="selectable font-mono text-xs text-warn">{t.install_hint}</span>
                    )}
                  </div>
                ))}
              </div>
              <div className="mt-3 flex items-center gap-2 text-[11px] text-fg-dim">
                <span className="font-mono text-fg-muted">v{__APP_VERSION__}</span> ·
                Settings are stored on this computer only (file mode 0600).
              </div>
            </Card>
          </div>
        </div>
      )}
    </>
  );
}

const THEMES: { value: ThemePref; label: string; icon: typeof Sun }[] = [
  { value: "dark", label: "Dark", icon: Moon },
  { value: "light", label: "Light", icon: Sun },
  { value: "system", label: "System", icon: Monitor },
];

function Appearance() {
  const { theme, setTheme } = useNav();
  return (
    <Card title="Appearance" icon={<Palette />}>
      <div role="radiogroup" aria-label="Theme" className="grid grid-cols-3 gap-2.5">
        {THEMES.map((t) => {
          const active = theme === t.value;
          const Icon = t.icon;
          return (
            <button
              key={t.value}
              type="button"
              role="radio"
              aria-checked={active}
              data-testid={`theme-${t.value}`}
              onClick={() => setTheme(t.value)}
              className={cx(
                "group rounded-lg border p-1.5 text-left transition",
                active ? "border-fg/50 ring-3 ring-fg/10" : "border-line hover:border-line-strong",
              )}
            >
              <ThemePreview kind={t.value} />
              <div className="flex items-center gap-1.5 px-1 pt-2 pb-0.5 text-xs font-medium">
                <Icon className="size-3.5 text-fg-muted" />
                {t.label}
              </div>
            </button>
          );
        })}
      </div>
    </Card>
  );
}

/** Tiny static mock of the app in each theme. Fixed colours on purpose. */
function ThemePreview({ kind }: { kind: ThemePref }) {
  const pane = (dark: boolean) => (
    <div className={cx("flex h-full flex-1 gap-1 p-1.5", dark ? "bg-[#0b0a0c]" : "bg-[#f7f6f7]")}>
      <div className={cx("w-1/4 rounded-sm", dark ? "bg-[#19171b]" : "bg-[#eceaed]")} />
      <div className="flex flex-1 flex-col gap-1">
        <div className={cx("h-1.5 w-2/3 rounded-full", dark ? "bg-[#3a363d]" : "bg-[#d5d1d7]")} />
        <div className={cx("flex-1 rounded-sm border", dark ? "border-[#2a272d] bg-[#131215]" : "border-[#e6e3e7] bg-white")}>
          <div className="m-1 h-1 w-1/3 rounded-full bg-[#ff6a3d]" />
        </div>
      </div>
    </div>
  );
  return (
    <div className="flex h-16 overflow-hidden rounded-md border border-line">
      {kind === "system" ? (
        <>
          {pane(false)}
          {pane(true)}
        </>
      ) : (
        pane(kind === "dark")
      )}
    </div>
  );
}
