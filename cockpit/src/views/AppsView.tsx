import { ArrowRightLeft, Boxes } from "lucide-react";
import { MachineCard } from "../components/MachineCard";
import { Button, Callout, Card, CopyButton, EmptyState, InlineCode, PageHeader } from "../components/ui";
import { isWorker } from "../lib/api";
import { hasCap } from "../lib/caps";
import { useLive } from "../lib/live";
import { useSession } from "../lib/session";
import { PlatformApps } from "./PlatformAppsView";

export function AppsView() {
  const { settings, servers, select } = useSession();
  const { health } = useLive();
  if (isWorker(settings)) {
    const mains = servers.filter((s) => !isWorker(s));
    return (
      <>
        <PageHeader title="Apps" subtitle={`${settings.hostname} is a worker: it runs apps for its main server.`} />
        <div className="rounded-xl border border-dashed border-line-strong bg-surface/50">
          <EmptyState
            icon={<Boxes />}
            title="Deploy from your main server"
            action={
              mains[0] && (
                <Button variant="primary" onClick={() => void select(mains[0]!.agent_url)}>
                  Switch to {mains[0].hostname}
                </Button>
              )
            }
          >
            Pick this machine under “Run on” when you create an app there, or let the main server place apps on it.
          </EmptyState>
        </div>
      </>
    );
  }
  // Coolify still runs this server's apps until `vercelify migrate`.
  if (health.data && !hasCap(health.data, "platform")) return <NotMigrated hostname={settings.hostname} />;
  return <PlatformApps />;
}

const STEPS: { cmd: string; what: string }[] = [
  { cmd: "sudo vercelify update", what: "Get the version that can migrate." },
  { cmd: "sudo vercelify migrate", what: "See what would happen: every app, database and service, nothing changed yet." },
  { cmd: "sudo vercelify migrate --apply", what: "Move them. Coolify's built images are reused, so nothing rebuilds; the switch takes about a minute." },
];

/** A server whose apps Coolify still runs: how to move them. */
function NotMigrated({ hostname }: { hostname: string }) {
  return (
    <>
      <PageHeader title="Apps" subtitle={`Coolify still runs ${hostname}'s apps. Vercelify's own engine takes them over in one step.`} />
      <div className="grid gap-6 @4xl:grid-cols-[minmax(0,1fr)_300px]">
        <Card
          title="Move this server to Vercelify's engine"
          description="Push to deploy, previews, sleeping apps and the rest of this page need it. Coolify is kept until you remove it, so you can go back."
          icon={<ArrowRightLeft />}
        >
          <ol className="space-y-3" data-testid="apps-migrate">
            {STEPS.map((s, i) => (
              <li key={s.cmd} className="flex items-start gap-3">
                <span className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full border border-line-strong text-[11px] text-fg-muted tabular-nums">
                  {i + 1}
                </span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2">
                    <InlineCode>{s.cmd}</InlineCode>
                    <CopyButton text={s.cmd} />
                  </div>
                  <p className="mt-1 text-[12px] text-fg-muted">{s.what}</p>
                </div>
              </li>
            ))}
          </ol>
          <Callout tone="idle" className="mt-4" title="If something isn't right">
            <InlineCode>sudo vercelify migrate --rollback</InlineCode> starts Coolify again as it was. Once every app answers, <InlineCode>sudo vercelify migrate --finish</InlineCode> removes Coolify and frees its memory.
          </Callout>
        </Card>
        <MachineCard />
      </div>
    </>
  );
}
