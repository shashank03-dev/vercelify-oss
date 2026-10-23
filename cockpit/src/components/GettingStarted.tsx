import { Check, X } from "lucide-react";
import { useState } from "react";
import { hostOf, primaryUrl } from "../lib/format";
import { useLive } from "../lib/live";
import { useNav } from "../lib/nav";
import { useSession } from "../lib/session";
import type { CoolifyApplication } from "../types";
import { Button, cx, IconButton } from "./ui";

const KEY = "vercelify.getting-started.hidden";

function hiddenFor(agentUrl: string): boolean {
  try {
    return (JSON.parse(localStorage.getItem(KEY) ?? "[]") as string[]).includes(agentUrl);
  } catch {
    return false;
  }
}

function hide(agentUrl: string) {
  try {
    const list = JSON.parse(localStorage.getItem(KEY) ?? "[]") as string[];
    localStorage.setItem(KEY, JSON.stringify([...new Set([...list, agentUrl])]));
  } catch {
    /* private mode */
  }
}

/** Addresses Coolify or Tailscale hand out: not a domain the user chose. */
const GENERATED = /(sslip\.io|nip\.io|\.ts\.net|localhost)$|^\d+\.\d+\.\d+\.\d+$/;

export function hasOwnDomain(apps: CoolifyApplication[]): boolean {
  return apps.some((a) => {
    const url = primaryUrl(a.fqdn);
    return !!url && !GENERATED.test(hostOf(url));
  });
}

/**
 * The first things to do on a fresh server, in order, each with the button
 * that does it. Ticks itself off from live data and goes away once
 * everything is done or the user closes it.
 */
export function GettingStarted({
  apps,
  onNewApp,
  onDomains,
}: {
  apps: CoolifyApplication[] | undefined;
  onNewApp: () => void;
  /** Open an app on its Domains tab. */
  onDomains: (app: CoolifyApplication) => void;
}) {
  const { settings, servers, addServer } = useSession();
  const { alerts } = useLive();
  const { go } = useNav();
  const [hidden, setHidden] = useState(() => hiddenFor(settings.agent_url));

  if (hidden || !apps) return null;

  const steps = [
    { title: "Connect a machine", detail: `${settings.hostname} is ready for deployments.`, done: true },
    {
      title: "Deploy your first app",
      detail: "Paste a Git repository. Every push to its branch deploys again.",
      done: apps.length > 0,
      action: { label: "New app", run: onNewApp },
    },
    {
      title: "Put an app on your own domain",
      detail: "HTTPS certificates are issued automatically.",
      done: hasOwnDomain(apps),
      action: apps[0] && { label: "Add a domain", run: () => onDomains(apps[0]!) },
    },
    {
      title: "Get alerts on your phone",
      detail: "Hear about failed deploys, full disks or low battery.",
      done: !!alerts.data?.notify_target,
      action: { label: "Set up alerts", run: () => go("settings") },
    },
    {
      title: "Add another machine",
      detail: "Optional: a second laptop or a Raspberry Pi to share the load.",
      done: servers.length > 1,
      action: { label: "Add a machine", run: addServer },
    },
  ];
  const done = steps.filter((s) => s.done).length;
  if (done === steps.length) return null;
  const next = steps.findIndex((s) => !s.done);

  return (
    <section className="mb-6 rounded-xl border border-line bg-surface" data-testid="getting-started">
      <header className="flex items-center gap-3 border-b border-line px-4 py-2.5">
        <div className="min-w-0 flex-1">
          <h2 className="text-[13px] font-medium">Get started</h2>
          <p className="text-xs text-fg-dim">
            {done} of {steps.length} done. Each step takes a minute.
          </p>
        </div>
        <div className="hidden h-1 w-28 overflow-hidden rounded-full bg-fg/[0.08] sm:block" aria-hidden>
          <div className="h-full rounded-full bg-accent transition-[width] duration-500" style={{ width: `${(done / steps.length) * 100}%` }} />
        </div>
        <IconButton
          label="Hide this guide"
          onClick={() => {
            hide(settings.agent_url);
            setHidden(true);
          }}
        >
          <X />
        </IconButton>
      </header>
      <ol className="divide-y divide-line">
        {steps.map((s, i) =>
          s.done ? null : (
            <li key={s.title} className="flex items-center gap-3 px-4 py-2.5">
              <span
                className={cx(
                  "flex size-5 shrink-0 items-center justify-center rounded-full border text-[10px] font-semibold tabular-nums",
                  i === next ? "border-fg/40 text-fg" : "border-line-strong text-fg-dim",
                )}
              >
                {i + 1}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block text-[13px] font-medium text-fg">{s.title}</span>
                <span className="block truncate text-xs text-fg-muted">{s.detail}</span>
              </span>
              {s.action && (
                <Button size="sm" variant={i === next ? "primary" : "subtle"} onClick={s.action.run}>
                  {s.action.label}
                </Button>
              )}
            </li>
          ),
        )}
      </ol>
      {done > 0 && (
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-line px-4 py-2 text-xs text-fg-dim">
          {steps
            .filter((s) => s.done)
            .map((s) => (
              <span key={s.title} className="inline-flex items-center gap-1">
                <Check className="size-3 text-ok" />
                {s.title}
              </span>
            ))}
        </p>
      )}
    </section>
  );
}
